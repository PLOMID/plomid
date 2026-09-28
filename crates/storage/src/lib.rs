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
//! Storage interfaces and storage engine implementations for PLOMID.

pub mod block_manager;
mod btree;
mod buffer_pool;
pub mod catalog;
pub mod checkpoint;
mod checksum;
mod codec;
pub mod device;
mod durable;
mod engine;
mod fs;
pub mod generation;
pub mod layout;
mod manager;
pub mod pack_manager;
mod page;
pub mod page_manager;
pub mod physical;
mod platform;
mod row;

pub use block_manager::BlockManager;

pub use btree::BTree;
pub use buffer_pool::{BufferPool, PageHandle};
pub use checkpoint::{
    create_checkpoint, create_checkpoint_with_fail_point, create_checkpoint_with_timings, discover,
    latest_valid, load_checkpoint, validate_storage_physical, CheckpointMetadata,
    CheckpointPhaseTimings, CheckpointRequest, FailPoint, CHECKPOINT_DIR_NAME,
    CHECKPOINT_FORMAT_VERSION, CHECKPOINT_HEADER_SIZE, CHECKPOINT_MAGIC, MAX_METADATA_SIZE,
};
pub use checksum::{compute, compute_checksum, verify, verify_checksum};
pub use device::{
    capacity_for_extents, default_device_capacity, device_record_path, read_device_record,
    read_device_record_optional, read_device_state, read_device_state_optional, register_device,
    round_up_to_extent, validate_capacity, AllocationTarget, DeviceAllocator, DeviceEntry,
    DeviceMetadata, DeviceRecord, DeviceRegistry, DeviceStateRecord, Extent, Lifecycle,
    StorageDevice, ALLOCATOR_FORMAT_VERSION, ALLOCATOR_MAGIC, DEVICE_FORMAT_VERSION,
    DEVICE_HEADER_LEN, DEVICE_MAGIC, DEVICE_META_HEADER_LEN, DEVICE_META_MAGIC,
    DEVICE_META_VERSION, DEVICE_STATE_HEADER_LEN, DEVICE_STATE_MAGIC, DEVICE_STATE_VERSION,
    MAX_DEVICE_CAPACITY, MIN_DEVICE_CAPACITY, SNAPSHOT_ENTRY_LEN, SNAPSHOT_PAGE_LEN,
    SNAPSHOT_PREFIX_LEN,
};
pub use engine::{CommitResult, StorageEngine, StorageEngineTransaction, TransactionState};
pub use fs::{FileSystem, RealFs};
pub use manager::{has_durable_state, storage_generation, Segment, SegmentState, StorageManager};
pub use pack_manager::PackManager;
pub use page::{
    Page, ValidatedPage, PAGE_DATA_SIZE, PAGE_HEADER_SIZE, PAGE_SIZE, PAGE_TRAILER_SIZE,
};
pub use page_manager::{PageManager, PageMetadata};
pub use physical::{
    BlockDirectoryEntry, BlockMetadata, IntegrityCheck, PackFooter, PackHeader, PageHeader,
    PageType, PhysicalLocation, BLOCKS_PER_EXTENT, BLOCK_FORMAT_VERSION, BLOCK_MAGIC, BLOCK_SIZE,
    DEFAULT_STORAGE_SEGMENT_SIZE_BYTES, EXTENT_SIZE, PACK_FOOTER_MAGIC, PACK_FORMAT_VERSION,
    PACK_MAGIC, PACK_TARGET_SIZE, PAGES_PER_BLOCK, PAGES_PER_EXTENT, PAGE_FORMAT_VERSION,
    PAGE_MAGIC,
};
pub use row::{decode, encode, Field, Row};

// The layout layer is the authoritative filesystem structure of a database.
// Its path algebra, metadata records, and validation entry points are part of
// the storage crate's public surface.
//
// `SchemaMetadata` here is the *layout-local* database/schema directory record
// (`objects/databases/.../META.dat`); the storage catalog's schema state keeps
// its existing `plomid_storage::catalog::SchemaMetadata` name without change.
pub use layout::SchemaMetadata as LayoutSchemaMetadata;
pub use layout::{
    database_dir_name, database_from_dir_name, index_payload_file_name,
    index_payload_generation_from_file_name, is_legacy_layout, reject_legacy_layout, required_dirs,
    schema_dir_name, schema_from_dir_name, table_dir_name, table_from_dir_name, validate_devices,
    validate_generation, validate_generation_flat, validate_index, validate_root, validate_table,
    DatabaseLayout, DatabaseMetadata, IndexGenerationMetadata, IndexGenerationState,
    IndexGenerationTrigger, IndexMetadata, TableMetadata, INDEX_GENERATION_META_HEADER_LEN,
    INDEX_GENERATION_META_MAGIC, INDEX_GENERATION_META_VERSION, META_FILE_NAME,
};

pub use catalog::{
    catalog_dir, catalog_file_name, catalog_path, discover_catalogs,
    generation_from_catalog_file_name, latest_valid_catalog, load_catalog,
    load_catalog_for_generation, load_catalog_for_version, CatalogState, ObjectRecord,
    PhysicalReference, PhysicalStructure, PublicationState, SchemaColumn, SchemaMetadata,
    CATALOG_DIR_NAME, CATALOG_FORMAT_VERSION, CATALOG_GENERATION_DIGITS, CATALOG_HEADER_SIZE,
    CATALOG_MAGIC, MAX_CATALOG_RECORDS, MAX_PHYSICAL_REFERENCES, MAX_RETAINED_GENERATIONS,
    MAX_SCHEMA_COLUMNS, PHYSICAL_REFERENCE_LEN, SCHEMA_COLUMN_LEN,
};
pub use generation::{
    catalog_staged_path, discover_generation_ids, discover_generations, generation_path,
    generation_path_in_schema, generation_staged_path, generation_staged_path_in_schema,
    load_generation, load_publication_pointer, publication_pointer_path,
    publication_pointer_staged_path, GcFailPoint, GcOutcome, GenerationManager, GenerationMetadata,
    GenerationPublication, GenerationReader, ObjectChange, PublicationFailPoint,
    PublicationPhaseTimings, PublicationPointer, PublicationPointerSource, PublicationRequest,
    PublishedGeneration, RecoveryOutcome, GENERATION_FORMAT_VERSION, GENERATION_HEADER_SIZE,
    GENERATION_MAGIC, MAX_GENERATION_REFERENCES, MAX_PUBLICATION_OBJECTS,
    PUBLICATION_POINTER_FILE_NAME, PUBLICATION_POINTER_FORMAT_VERSION, PUBLICATION_POINTER_MAGIC,
    PUBLICATION_POINTER_SIZE,
};
