use super::error::Result;
use guppy::graph::PackageGraph;
use once_cell::sync::OnceCell;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct CoreContext {
    current_dir: PathBuf,
    package_graph: OnceCell<PackageGraph>,
}

impl CoreContext {
    pub fn new(current_dir: PathBuf) -> CoreContext {
        unimplemented!()
    }

    pub fn initialize(&mut self) -> Result<()> {
        unimplemented!()
    }

    pub fn current_dir(&self) -> &Path {
        unimplemented!()
    }

    pub fn package_graph(&self) -> Result<&PackageGraph> {
        unimplemented!()
    }

    pub fn project_root(&self) -> &Path {
        unimplemented!()
    }

    fn try_build_package_graph(&self) -> Result<PackageGraph> {
        unimplemented!()
    }
}
