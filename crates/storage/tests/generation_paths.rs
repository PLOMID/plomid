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
//! Recovery validates generation identity against the logical directory tree.

use plomid_core::{
    CatalogVersion, DatabaseId, ErrorKind, GenerationId, Lsn, ObjectId, SchemaId, TableId,
};
use plomid_storage::{
    discover_generations, load_generation, DatabaseLayout, GenerationManager, GenerationMetadata,
    PublicationState,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_DATABASE_ID: DatabaseId = DatabaseId::new(1);
static TEST_SCHEMA_ID: SchemaId = SchemaId::new(1);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "plomid-generation-paths-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn record() -> GenerationMetadata {
    GenerationMetadata::new(
        GenerationId::new(7),
        ObjectId::new(9),
        CatalogVersion::new(1),
        GenerationId::new(1),
        GenerationId::new(1),
        Lsn::new(1),
        None,
        PublicationState::Published,
        vec![],
    )
    .unwrap()
}

#[test]
fn recovery_rejects_metadata_under_another_table() {
    let root = Scratch::new();
    let layout = DatabaseLayout::new(&root.0);
    let metadata = record();
    let wrong_table = TableId::new(10);
    layout
        .ensure_generation_dir(
            TEST_DATABASE_ID,
            TEST_SCHEMA_ID,
            wrong_table,
            metadata.generation_id,
        )
        .unwrap();
    std::fs::write(
        layout.generation_meta_path(
            TEST_DATABASE_ID,
            TEST_SCHEMA_ID,
            wrong_table,
            metadata.generation_id,
        ),
        metadata.encode().unwrap(),
    )
    .unwrap();
    assert_eq!(
        load_generation(&root.0, metadata.generation_id)
            .unwrap_err()
            .kind(),
        ErrorKind::Corruption
    );
}

#[test]
fn discovery_rejects_duplicate_global_generation_ids() {
    let root = Scratch::new();
    let layout = DatabaseLayout::new(&root.0);
    let metadata = record();
    for table in [TableId::new(9), TableId::new(10)] {
        layout
            .ensure_generation_dir(
                TEST_DATABASE_ID,
                TEST_SCHEMA_ID,
                table,
                metadata.generation_id,
            )
            .unwrap();
        std::fs::write(
            layout.generation_meta_path(
                TEST_DATABASE_ID,
                TEST_SCHEMA_ID,
                table,
                metadata.generation_id,
            ),
            metadata.encode().unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        discover_generations(&root.0).unwrap_err().kind(),
        ErrorKind::Corruption
    );
}

#[test]
fn recovery_refuses_old_generation_locations_without_modifying_data() {
    for marker in ["generation", "generations", "catalog/CURRENT"] {
        let root = Scratch::new();
        let path = root.0.join(marker);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"legacy metadata").unwrap();
        assert!(GenerationManager::open(&root.0).is_err());
        assert!(GenerationManager::recover_if_present(&root.0).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"legacy metadata");
        assert!(!root.0.join("objects").exists());
        assert!(!root.0.join("CURRENT").exists());
    }
}
