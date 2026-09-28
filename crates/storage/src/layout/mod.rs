#![forbid(unsafe_code)]
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
//! Filesystem layout of a PLOMID database directory.
//!
//! A database lives in one root directory (`PLOMID_DATA/`) that separates two
//! concerns which are deliberately never mixed:
//!
//! ```text
//! PLOMID_DATA/
//! ├── CURRENT
//! ├── catalog/
//! ├── wal/
//! ├── checkpoints/
//! ├── objects/
//! │   ├── tables/
//! │   │   └── T-0000000000000001/
//! │   │       ├── META.dat
//! │   │       ├── hot/
//! │   │       ├── generations/
//! │   │       │   └── GEN-0000000000000001/
//! │   │       │       ├── META.dat
//! │   │       │       └── segments/
//! │   │       └── indexes/
//! │   │           └── I-0000000000000001/
//! │   │               ├── META.dat
//! │   │               └── GEN-0000000000000001.dat
//! │   └── databases/
//! │       └── DB-0000000000000001/
//! │           ├── META.dat
//! │           └── schemas/
//! │               └── S-0000000000000001/
//! │                   ├── META.dat
//! │                   └── tables/
//! ├── devices/
//! │   ├── D-0000000000000001/
//! │   │   ├── DEVICE.dat
//! │   │   ├── packs/
//! │   │   ├── free/
//! │   │   └── state/
//! │   └── D-0000000000000002/
//! │       ├── DEVICE.dat
//! │       ├── packs/
//! │       ├── free/
//! │       └── state/
//! ├── shared/
//! │   ├── tx/
//! │   ├── rowid/
//! │   ├── statistics/
//! │   ├── filters/
//! │   └── dictionaries/
//! ├── temp/
//! └── lost+found/
//! ```
//!
//! `objects/tables/` is the durable home of table storage objects: generations,
//! placement metadata, and indexes keep their existing table-local paths so
//! recovery, validation, GC, and multi-device placement do not change.
//! `objects/databases/` is the durable home of logical database/schema objects
//! created through the SQL catalog path. The SQL catalog remains the name
//! resolution authority; these directories materialize logical object identity
//! without duplicating catalog state or moving physical bytes.
//!
//! # Logical versus physical
//!
//! `objects/` describes *what* the database contains. `devices/` holds the
//! *capacity* those objects are stored in. A logical table never owns physical
//! pages: its generations reference physical segments, and the storage layer
//! places those segments on devices. A table may therefore span several
//! devices, and the capacity of the database is the sum of its registered
//! device capacities rather than the capacity of any single device.
//!
//! # Module map
//!
//! * [`names`] – shared naming primitives of the layout (identity rendering).
//! * `root` – [`DatabaseLayout`] and the root-level directories.
//! * `current` – the durable pointer to the published state.
//! * `databases` – `objects/databases/DB-*/` and database `META.dat`.
//! * `schemas` – `objects/databases/DB-*/schemas/S-*/` and schema `META.dat`.
//! * `tables` – `objects/tables/T-*/` and table `META.dat`.
//! * `generations` – `objects/tables/T-*/generations/GEN-*/`.
//! * `indexes` – `objects/tables/T-*/indexes/I-*/`.
//! * `devices` – `devices/D-*/` paths.
//! * `shared` – `shared/{tx,rowid,statistics,filters,dictionaries}/`.
//! * `metadata` – durable metadata framing and atomic publication.
//! * `validation` – structural validation of the layout.
//! * `legacy` – detection of storage layouts this build does not adopt.
//!
//! The layout module owns filesystem *names and paths* and nothing else: it
//! never allocates storage, chooses a device, writes pages, manages
//! transactions, performs WAL or recovery work, or owns catalog state. Those
//! responsibilities stay with the components that already implement them.

mod current;
mod databases;
mod devices;
mod generations;
mod indexes;
pub mod legacy;
pub(crate) mod metadata;
mod names;
mod root;
mod schemas;
mod shared;
mod tables;
mod validation;

pub use databases::{database_dir_name, database_from_dir_name, DatabaseMetadata};
pub use devices::{device_dir_name, device_from_dir_name};
pub use generations::{generation_dir_name, generation_from_dir_name};
pub use indexes::{
    index_dir_name, index_from_dir_name, index_generation_file_name,
    index_generation_from_file_name, index_payload_file_name,
    index_payload_generation_from_file_name, IndexGenerationMetadata, IndexGenerationState,
    IndexGenerationTrigger, IndexMetadata, INDEX_GENERATION_META_HEADER_LEN,
    INDEX_GENERATION_META_MAGIC, INDEX_GENERATION_META_VERSION, INDEX_META_MAGIC,
    INDEX_META_VERSION,
};
pub use legacy::{is_legacy_layout, reject_legacy_layout};
pub use names::{render_layout_id, LAYOUT_ID_DIGITS, META_FILE_NAME, STAGING_SUFFIX};
pub use root::DatabaseLayout;
pub use schemas::{schema_dir_name, schema_from_dir_name, SchemaMetadata};
pub use shared::{
    SHARED_DICTIONARIES_DIR_NAME, SHARED_FILTERS_DIR_NAME, SHARED_ROWID_DIR_NAME,
    SHARED_STATISTICS_DIR_NAME, SHARED_TX_DIR_NAME,
};
pub use tables::{
    table_dir_name, table_from_dir_name, TableMetadata, TABLE_META_MAGIC, TABLE_META_VERSION,
};
pub use validation::{
    required_dirs, validate_devices, validate_generation, validate_generation_flat, validate_index,
    validate_root, validate_table,
};
