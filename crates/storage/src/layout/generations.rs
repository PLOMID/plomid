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
//! Generation directories: `objects/databases/DB-<db id>/schemas/S-<schema id>/tables/T-<table id>/generations/GEN-<id>/`.
//!
//! Every immutable generation of a table lives inside the table's own directory:\n//!
//! ```text
//! objects/databases/DB-00000000000000000001/schemas/S-00000000000000000001/tables/T-0000000000000001/generations/GEN-0000000000000001/
//! ├── META.dat    immutable generation metadata
//! └── segments/   segment placement directory of the generation
//! ```
//!
//! There is no database-level generation directory: a generation belongs to
//! exactly one logical object, so it is stored inside that object's table
//! directory and nowhere else. The generation *lifecycle* (publication,
//! retention, reader snapshots, reclamation) remains owned by the storage
//! crate's generation manager; this module owns only where those bytes live.
//!
//! `segments/` belongs to the generation but never holds physical pages: the
//! storage layer places segment containers on physical devices
//! (`devices/D-*/packs/`), and a generation's metadata carries the physical
//! references that resolve them. Keeping physical bytes out of the logical tree
//! is what allows one table, and therefore one generation, to span several
//! devices.

use super::metadata;
use super::names::{parse_layout_id, render_prefixed_id};
use super::DatabaseLayout;
use crate::durable;
use plomid_core::{DatabaseId, ErrorKind, GenerationId, PlomidError, Result, SchemaId, TableId};
use std::path::PathBuf;

/// Directory holding the generations of one table.
pub const GENERATIONS_DIR_NAME: &str = "generations";
/// Prefix of a generation directory name.
pub const GENERATION_DIR_PREFIX: &str = "GEN-";
/// Directory holding the segment placement of one generation.
pub const GENERATION_SEGMENTS_DIR_NAME: &str = "segments";

/// Renders the directory name of a generation.
#[must_use]
pub fn generation_dir_name(generation: GenerationId) -> String {
    render_prefixed_id(GENERATION_DIR_PREFIX, generation.get())
}

/// Parses a generation directory name. Only the deterministic form is accepted.
#[must_use]
pub fn generation_from_dir_name(name: &str) -> Option<GenerationId> {
    parse_layout_id(name, GENERATION_DIR_PREFIX).map(GenerationId::new)
}

impl DatabaseLayout {
    /// Directory of one immutable generation of one table, inside its schema.
    #[must_use]
    pub fn generation_dir(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        generation: GenerationId,
    ) -> PathBuf {
        self.table_generations_dir_in_schema(database_id, schema_id, table_id)
            .join(generation_dir_name(generation))
    }

    /// Metadata record of one immutable generation, inside its schema.
    #[must_use]
    pub fn generation_meta_path(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        generation: GenerationId,
    ) -> PathBuf {
        self.generation_dir(database_id, schema_id, table_id, generation)
            .join(super::names::META_FILE_NAME)
    }

    /// Staging path used while publishing generation metadata, inside its schema.
    #[must_use]
    pub fn generation_staged_path(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        generation: GenerationId,
    ) -> PathBuf {
        metadata::staged_path(&self.generation_meta_path(
            database_id,
            schema_id,
            table_id,
            generation,
        ))
    }

    /// Segment placement directory of one generation, inside its schema.
    #[must_use]
    pub fn generation_segments_dir(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        generation: GenerationId,
    ) -> PathBuf {
        self.generation_dir(database_id, schema_id, table_id, generation)
            .join(GENERATION_SEGMENTS_DIR_NAME)
    }

    /// Directory of one generation in the flat storage tree.
    ///
    /// The flat tree (`objects/tables/T-*/generations/GEN-*/`) is the durable
    /// location of generation objects: discovery, recovery, validation, and
    /// GC enumerate it directly. The SQL-facing logical tree under
    /// `objects/databases/` materializes database/schema/table identity; the
    /// owning identity of each generation is carried by `ObjectChange` /
    /// `TableIdentity` and mapped to these flat paths at publication time.
    #[must_use]
    pub fn generation_dir_flat(&self, table_id: TableId, generation: GenerationId) -> PathBuf {
        self.table_generations_dir(table_id)
            .join(generation_dir_name(generation))
    }

    /// Metadata record of one generation in the flat storage tree.
    #[must_use]
    pub fn generation_meta_path_flat(
        &self,
        table_id: TableId,
        generation: GenerationId,
    ) -> PathBuf {
        self.generation_dir_flat(table_id, generation)
            .join(super::names::META_FILE_NAME)
    }

    /// Staging path used while publishing generation metadata in the flat tree.
    #[must_use]
    pub fn generation_staged_path_flat(
        &self,
        table_id: TableId,
        generation: GenerationId,
    ) -> PathBuf {
        metadata::staged_path(&self.generation_meta_path_flat(table_id, generation))
    }

    /// Segment placement directory of one generation in the flat storage tree.
    #[must_use]
    pub fn generation_segments_dir_flat(
        &self,
        table_id: TableId,
        generation: GenerationId,
    ) -> PathBuf {
        self.generation_dir_flat(table_id, generation)
            .join(GENERATION_SEGMENTS_DIR_NAME)
    }

    /// Creates the directory of a generation and its segment placement
    /// directory, without touching existing metadata.
    pub fn ensure_generation_dir(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        generation: GenerationId,
    ) -> Result<PathBuf> {
        if generation.is_zero() {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "generation identity is zero",
            ));
        }
        let dir = self.generation_dir(database_id, schema_id, table_id, generation);
        // The table directory is materialized first so a generation can never
        // exist without its owning logical object.
        self.ensure_table_in_schema(database_id, schema_id, table_id)?;
        durable::ensure_dir(&dir)?;
        let segs = self.generation_segments_dir(database_id, schema_id, table_id, generation);
        durable::ensure_dir(&segs)?;
        Ok(dir)
    }

    /// Creates the directory of a generation in the flat storage tree,
    /// without touching existing metadata.
    ///
    /// Used by the internal publication flow, which resolves the owning
    /// `TableIdentity` before deriving the flat path.
    pub fn ensure_generation_dir_flat(
        &self,
        table_id: TableId,
        generation: GenerationId,
    ) -> Result<PathBuf> {
        if generation.is_zero() {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "generation identity is zero",
            ));
        }
        let dir = self.generation_dir_flat(table_id, generation);
        self.ensure_table(table_id)?;
        durable::ensure_dir(&dir)?;
        durable::ensure_dir(&self.generation_segments_dir_flat(table_id, generation))?;
        Ok(dir)
    }

    /// Enumerates published generation metadata across the database hierarchy.
    ///
    /// Discovery walks `objects/databases/<DB>/schemas/<S>/tables/<T>/generations/`
    /// for every table that actually exists, and reports the generation metadata
    /// files it finds. Results are ordered by generation identity so recovery and
    /// reclamation observe a deterministic order.
    pub fn discover_generation_files(&self) -> Result<Vec<(GenerationId, PathBuf)>> {
        let mut found = Vec::new();
        for database_id in self.discover_database_ids()? {
            for schema_id in self.discover_schema_ids(database_id)? {
                for table_id in self.discover_table_ids_in_schema(database_id, schema_id)? {
                    let dir =
                        self.table_generations_dir_in_schema(database_id, schema_id, table_id);
                    self.collect_generation_files_in(&dir, &mut found)?;
                }
            }
        }
        self.collect_flat_generation_files(&mut found)?;
        found.sort_unstable_by_key(|(generation, _)| *generation);
        Ok(found)
    }

    fn collect_generation_files_in(
        &self,
        dir: &std::path::Path,
        found: &mut Vec<(GenerationId, PathBuf)>,
    ) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir).map_err(PlomidError::from)? {
            let entry = entry.map_err(PlomidError::from)?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(generation) = generation_from_dir_name(&name) else {
                continue;
            };
            let meta_path = entry.path().join(super::names::META_FILE_NAME);
            if meta_path.is_file() {
                found.push((generation, meta_path));
            }
        }
        Ok(())
    }

    fn collect_flat_generation_files(
        &self,
        found: &mut Vec<(GenerationId, PathBuf)>,
    ) -> Result<()> {
        let flat_tables_dir = self.tables_dir();
        if !flat_tables_dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&flat_tables_dir).map_err(PlomidError::from)? {
            let entry = entry.map_err(PlomidError::from)?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(table_id) = super::tables::table_from_dir_name(&name) else {
                continue;
            };
            let dir = self.table_generations_dir(table_id);
            self.collect_generation_files_in(&dir, found)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{generation_dir_name, generation_from_dir_name};
    use plomid_core::GenerationId;

    #[test]
    fn directory_names_round_trip_through_identity() {
        assert_eq!(
            generation_dir_name(GenerationId::new(7)),
            "GEN-00000000000000000007"
        );
        assert_eq!(
            generation_from_dir_name("GEN-00000000000000000007"),
            Some(GenerationId::new(7))
        );
        assert_eq!(generation_from_dir_name("GEN-7"), None);
        assert_eq!(generation_from_dir_name("generation-7.gen"), None);
    }
}
