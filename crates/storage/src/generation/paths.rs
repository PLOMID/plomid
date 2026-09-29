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
//! Persistence paths and discovery for table-owned generation metadata.

use super::GenerationMetadata;
use crate::codec::corruption;
use crate::layout::{generation_dir_name, table_dir_name, DatabaseLayout};
use plomid_core::{DatabaseId, ErrorKind, GenerationId, PlomidError, Result, SchemaId, TableId};
use std::path::{Path, PathBuf};

/// Published metadata path of a generation inside the flat storage tree.
///
/// This is the pre-hierarchy path (`objects/tables/T-*/generations/GEN-*/`)
/// kept for the internal publication flow only; the SQL-facing hierarchy
/// lives under `objects/databases/` and is resolved through `ObjectChange`
/// → `TableIdentity` before these flat paths are derived.
#[must_use]
pub fn generation_path(root: &Path, table: TableId, generation: GenerationId) -> PathBuf {
    DatabaseLayout::new(root).generation_meta_path_flat(table, generation)
}

/// Staging sibling of the flat table-local metadata record.
#[must_use]
pub fn generation_staged_path(root: &Path, table: TableId, generation: GenerationId) -> PathBuf {
    DatabaseLayout::new(root).generation_staged_path_flat(table, generation)
}

/// Published metadata path inside the database/schema/table hierarchy.
/// This is the production path for generation metadata once the hierarchy
/// migration is complete.
#[must_use]
pub fn generation_path_in_schema(
    root: &Path,
    database_id: DatabaseId,
    schema_id: SchemaId,
    table_id: TableId,
    generation: GenerationId,
) -> PathBuf {
    DatabaseLayout::new(root).generation_meta_path(database_id, schema_id, table_id, generation)
}

/// Staging sibling of the hierarchy-aware metadata record.
#[must_use]
pub fn generation_staged_path_in_schema(
    root: &Path,
    database_id: DatabaseId,
    schema_id: SchemaId,
    table_id: TableId,
    generation: GenerationId,
) -> PathBuf {
    DatabaseLayout::new(root).generation_staged_path(database_id, schema_id, table_id, generation)
}

/// Enumerates published generation records, rejecting duplicate global IDs.
/// Generation identity remains database-wide, as required by the catalog.
pub fn discover_generations(root: &Path) -> Result<Vec<(GenerationId, PathBuf)>> {
    let files = DatabaseLayout::new(root).discover_generation_files()?;
    for pair in files.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(corruption("duplicate generation identity across tables"));
        }
    }
    Ok(files)
}

/// Enumerates published identities in deterministic order.
pub fn discover_generation_ids(root: &Path) -> Result<Vec<GenerationId>> {
    Ok(discover_generations(root)?
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// Reads a generation and validates its identity against its table-local path.
/// This discovery entry point is used by explicit recovery and inspection, not
/// by the physical allocation path.
///
/// The generation may live either in the flat tree
/// (`objects/tables/T-<object>/generations/GEN-<id>/`) or inside the logical
/// hierarchy (`objects/databases/DB-*/schemas/S-*/tables/T-<object>/generations/…`);
/// both are table-local, so validation checks that the discovered directory is
/// named after the generation and owned by the table the metadata names.
pub fn load_generation(root: &Path, generation: GenerationId) -> Result<GenerationMetadata> {
    let (_, path) = discover_generations(root)?
        .into_iter()
        .find(|(id, _)| *id == generation)
        .ok_or_else(|| PlomidError::new(ErrorKind::NotFound, "generation metadata is missing"))?;
    let bytes = std::fs::read(&path).map_err(PlomidError::from)?;
    let metadata = GenerationMetadata::decode(&bytes)?;
    if metadata.generation_id != generation {
        return Err(corruption(
            "generation metadata identity disagrees with its table-local path",
        ));
    }
    let _ = root;
    let gen_dir_ok = path
        .parent()
        .map(|dir| {
            dir.file_name().map_or(false, |name| {
                name.to_string_lossy() == generation_dir_name(generation)
            })
        })
        .unwrap_or(false);
    // META.dat → GEN-<id>/ → generations/ → T-<table>/
    let table_dir_ok = path
        .parent()
        .and_then(|dir| dir.parent())
        .and_then(|dir| dir.parent())
        .and_then(|table_dir| table_dir.file_name())
        .map_or(false, |name| {
            name.to_string_lossy() == table_dir_name(TableId::new(metadata.object_id.get()))
        });
    if !gen_dir_ok || !table_dir_ok {
        return Err(corruption(
            "generation metadata identity disagrees with its table-local path",
        ));
    }
    Ok(metadata)
}
