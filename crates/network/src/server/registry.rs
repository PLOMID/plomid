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
//! Multi-database registry: durable list of logical databases plus helpers.

use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct DatabaseRegistry {
    pub(crate) names: std::collections::BTreeSet<String>,
}

impl DatabaseRegistry {
    pub(crate) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("databases.registry");
        let mut registry = Self::default();
        if let Ok(contents) = std::fs::read_to_string(path) {
            registry.names.extend(
                contents
                    .lines()
                    .filter_map(safe_database_name)
                    .map(str::to_owned),
            );
        }
        registry.names.insert("plomid".to_string());
        registry
    }

    pub(crate) fn persist(&self, data_dir: &Path) -> std::io::Result<()> {
        let mut contents = self.names.iter().cloned().collect::<Vec<_>>().join("\n");
        contents.push('\n');
        let temporary = data_dir.join("databases.registry.tmp");
        std::fs::write(&temporary, contents)?;
        let file = std::fs::OpenOptions::new().read(true).open(&temporary)?;
        file.sync_all()?;
        std::fs::rename(temporary, data_dir.join("databases.registry"))
    }

    pub(crate) fn open_path(&mut self, data_dir: &Path, name: &str) -> std::io::Result<PathBuf> {
        let name = safe_database_name(name).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid database name")
        })?;
        let actual_name = self
            .names
            .iter()
            .find(|registered| registered.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("database \"{name}\" does not exist"),
                )
            })?;
        let path = database_path(data_dir, actual_name);
        if !path.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("database \"{name}\" does not exist"),
            ));
        }
        Ok(path)
    }
}

pub(crate) fn safe_database_name(name: &str) -> Option<&str> {
    let mut chars = name.chars();
    if !matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(name)
}

pub(crate) fn database_path(data_dir: &Path, name: &str) -> PathBuf {
    if name.eq_ignore_ascii_case("plomid") {
        data_dir.to_path_buf()
    } else {
        data_dir.join("databases").join(name)
    }
}
