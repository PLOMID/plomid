//
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//! Structural validation of the database layout.
//!
//! Validation answers a bounded question: *is this directory a structural
//! PLOMID database, and are the required pieces of it present?* It reads only
//! directory listings and the small metadata records that identify a piece of
//! the layout. It never scans pages, blocks, packs, or segments, so it is an
//! explicit operation (or a startup check) rather than something performed on
//! every database operation.
//!
//! Validation is also where an incompatible legacy layout is refused: a
//! directory that carries the pre-`PLOMID_DATA` layout must fail loudly instead
//! of being interpreted as a partially valid new layout.
//!
//! Device *record* validation (the contents of `DEVICE.dat` and
//! `state/STATE.dat`) belongs to the device module, which owns those formats;
//! this module validates that each device directory is structurally present and
//! returns the devices it found so a caller can validate their records.

use super::legacy;
use super::DatabaseLayout;
use plomid_core::{
    DatabaseId, DeviceId, ErrorKind, GenerationId, IndexId, PlomidError, Result, SchemaId, TableId,
};
use std::path::{Path, PathBuf};

/// Directories that a valid database root must contain.
///
/// The order is the order in which initialization creates them and the order in
/// which validation checks them, so a missing directory is reported
/// deterministically.
///
/// `objects/databases/` is required because database/schema object metadata
/// lives there once created. Individual `DB-*/S-*` directories are lifecycle
/// state and are validated separately; initialization never fabricates logical
/// objects.
#[must_use]
pub fn required_dirs(layout: &DatabaseLayout) -> Vec<PathBuf> {
    vec![
        layout.catalog_dir(),
        layout.wal_dir(),
        layout.checkpoints_dir(),
        layout.objects_dir(),
        layout.tables_dir(),
        layout.databases_dir(),
        layout.devices_dir(),
        layout.shared_dir(),
        layout.shared_tx_dir(),
        layout.shared_rowid_dir(),
        layout.shared_statistics_dir(),
        layout.shared_filters_dir(),
        layout.shared_dictionaries_dir(),
        layout.temp_dir(),
        layout.lost_found_dir(),
    ]
}

/// Validates the structural layout of a database root.
///
/// Reports the first missing or misplaced required directory, and refuses a
/// root that carries an incompatible legacy layout.
pub fn validate_root(layout: &DatabaseLayout) -> Result<()> {
    legacy::reject_legacy_layout(layout)?;
    let root = layout.root();
    if !root.exists() {
        return Err(PlomidError::with_detail(
            ErrorKind::NotFound,
            "database root is missing",
            format!("root={}", root.display()),
        ));
    }
    if !root.is_dir() {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "database root is not a directory",
            format!("root={}", root.display()),
        ));
    }
    for dir in required_dirs(layout) {
        if !dir.is_dir() {
            return Err(PlomidError::with_detail(
                ErrorKind::NotFound,
                "database layout is missing a required directory",
                format!("path={}", relative(root, &dir)),
            ));
        }
    }
    validate_current(layout)?;
    Ok(())
}

/// Validates `CURRENT`, the durable pointer to the published state.
///
/// An absent pointer is valid: it means the database has never published a
/// state, which is exactly the state a freshly initialized database is in, so
/// nothing is written here that would claim a publication happened. A pointer
/// that *is* present must decode, pass its checksum and version checks, and
/// name a published state; a corrupt pointer is reported rather than ignored,
/// because ignoring it would let the database choose a state behind the
/// operator's back.
fn validate_current(layout: &DatabaseLayout) -> Result<()> {
    if !layout.current_exists() {
        return Ok(());
    }
    let bytes = super::metadata::read(&layout.current_path(), "publication pointer")?;
    crate::generation::PublicationPointer::decode(&bytes)?.validate()?;
    Ok(())
}

/// Validates the structural pieces of every registered device directory.
///
/// Never assumes a specific device exists: the devices are discovered from the
/// layout, and an empty device set is not an error here because only database
/// creation is responsible for registering the first device.
pub fn validate_devices(layout: &DatabaseLayout) -> Result<Vec<DeviceId>> {
    let ids = layout.discover_device_ids()?;
    for device_id in &ids {
        for dir in [
            layout.device_dir(*device_id),
            layout.device_packs_dir(*device_id),
            layout.device_free_dir(*device_id),
            layout.device_state_dir(*device_id),
        ] {
            if !dir.is_dir() {
                return Err(PlomidError::with_detail(
                    ErrorKind::Corruption,
                    "device directory is not structurally valid",
                    format!(
                        "device_id={} path={}",
                        device_id.get(),
                        relative(layout.root(), &dir)
                    ),
                ));
            }
        }
    }
    Ok(ids)
}

/// Validates one materialized table directory.
pub fn validate_table(
    layout: &DatabaseLayout,
    database_id: DatabaseId,
    schema_id: SchemaId,
    table_id: TableId,
) -> Result<()> {
    if !layout
        .table_dir_in_schema(database_id, schema_id, table_id)
        .is_dir()
    {
        return Err(PlomidError::with_detail(
            ErrorKind::NotFound,
            "table directory is missing",
            format!("table_id={}", table_id.get()),
        ));
    }
    layout.read_table_meta_in_schema(database_id, schema_id, table_id)?;
    for dir in [
        layout.table_hot_dir_in_schema(database_id, schema_id, table_id),
        layout.table_generations_dir_in_schema(database_id, schema_id, table_id),
        layout.table_indexes_dir_in_schema(database_id, schema_id, table_id),
    ] {
        if !dir.is_dir() {
            return Err(PlomidError::with_detail(
                ErrorKind::Corruption,
                "table directory is not structurally valid",
                format!(
                    "table_id={} path={}",
                    table_id.get(),
                    relative(layout.root(), &dir)
                ),
            ));
        }
    }
    Ok(())
}

/// Validates one materialized generation directory.
///
/// The generation's metadata record is decoded, which establishes its format,
/// checksum, and identity. Cross-record relationships (catalog version,
/// publication, physical references) are validated by the generation manager,
/// which owns them.
pub fn validate_generation(
    layout: &DatabaseLayout,
    database_id: DatabaseId,
    schema_id: SchemaId,
    table_id: TableId,
    generation: GenerationId,
) -> Result<()> {
    let dir = layout.generation_dir(database_id, schema_id, table_id, generation);
    if !dir.is_dir() {
        return Err(PlomidError::with_detail(
            ErrorKind::NotFound,
            "generation directory is missing",
            format!(
                "table_id={} generation_id={} path={}",
                table_id.get(),
                generation.get(),
                relative(layout.root(), &dir)
            ),
        ));
    }
    if !layout
        .generation_segments_dir(database_id, schema_id, table_id, generation)
        .is_dir()
    {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "generation directory has no segment placement directory",
            format!(
                "table_id={} generation_id={}",
                table_id.get(),
                generation.get()
            ),
        ));
    }
    let bytes = super::metadata::read(
        &layout.generation_meta_path(database_id, schema_id, table_id, generation),
        "generation metadata",
    )?;
    let metadata = crate::generation::GenerationMetadata::decode(&bytes)?;
    if metadata.generation_id != generation {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "generation metadata identity does not match its directory",
            format!("generation_id={}", metadata.generation_id.get()),
        ));
    }
    Ok(())
}

/// Validates one materialized generation directory in the flat storage tree.
///
/// The flat tree (`objects/tables/T-*/generations/GEN-*/`) is the durable
/// location of generation objects used by publication, recovery, and GC; this
/// entry point validates it directly without requiring the SQL-facing identity
/// hierarchy to be resolved first.
pub fn validate_generation_flat(
    layout: &DatabaseLayout,
    table_id: TableId,
    generation: GenerationId,
) -> Result<()> {
    let dir = layout.generation_dir_flat(table_id, generation);
    if !dir.is_dir() {
        return Err(PlomidError::with_detail(
            ErrorKind::NotFound,
            "generation directory is missing",
            format!(
                "table_id={} generation_id={} path={}",
                table_id.get(),
                generation.get(),
                relative(layout.root(), &dir)
            ),
        ));
    }
    if !layout
        .generation_segments_dir_flat(table_id, generation)
        .is_dir()
    {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "generation directory has no segment placement directory",
            format!(
                "table_id={} generation_id={}",
                table_id.get(),
                generation.get()
            ),
        ));
    }
    let bytes = super::metadata::read(
        &layout.generation_meta_path_flat(table_id, generation),
        "generation metadata",
    )?;
    let metadata = crate::generation::GenerationMetadata::decode(&bytes)?;
    if metadata.generation_id != generation {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "generation metadata identity does not match its directory",
            format!("generation_id={}", metadata.generation_id.get()),
        ));
    }
    Ok(())
}

/// Validates one materialized index directory.
pub fn validate_index(
    layout: &DatabaseLayout,
    database_id: DatabaseId,
    schema_id: SchemaId,
    table_id: TableId,
    index_id: IndexId,
) -> Result<()> {
    if !layout
        .index_dir_in_schema(database_id, schema_id, table_id, index_id)
        .is_dir()
    {
        return Err(PlomidError::with_detail(
            ErrorKind::NotFound,
            "index directory is missing",
            format!("table_id={} index_id={}", table_id.get(), index_id.get()),
        ));
    }
    let metadata = layout.read_index_meta_in_schema(database_id, schema_id, table_id, index_id)?;
    if metadata.table_id != table_id || metadata.index_id != index_id {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "index metadata does not describe this index directory",
            format!("index_id={}", index_id.get()),
        ));
    }
    Ok(())
}

/// Renders `path` relative to `root` for diagnostics without leaking the
/// absolute host path of the database.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.display().to_string(),
        |rest| rest.display().to_string(),
    )
}
