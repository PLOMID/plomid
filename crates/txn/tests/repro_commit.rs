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
use plomid_txn::{PlomidStorageEngine, StorageEngine, StorageEngineTransaction};

#[test]
fn repro_commit_not_found_on_existing_data_dir() {
    let root = std::path::Path::new("/tmp/plomid-data-repro");
    let mut engine =
        PlomidStorageEngine::open_with_config(root, root, 64, 8 * 1024 * 1024, 8 * 1024 * 1024)
            .expect("open");
    let mut txn = engine.begin().expect("begin");
    let r0 = txn.commit();
    eprintln!("empty commit: {r0:?}");
    let mut txn = engine.begin().expect("begin2");
    txn.put(b"repro-key", b"repro-value").expect("put");
    txn.delete(b"no-such-key").expect("delete");
    let result = txn.commit();

    match &result {
        Err(e) => eprintln!("COMMIT FAILED: kind={:?} msg={}", e.kind(), e),
        Ok(r) => eprintln!("COMMIT OK ts={:?}", r.commit_timestamp),
    }
    result.expect("commit");
}
