use super::error::{Result, SeamareError};
use camino::Utf8Path;
use guppy::graph::PackageGraph;
use guppy::MetadataCommand;

// TODO: maybe it's better to store `project_root` field like what x-linter does.
// but it's borrowed from inner `package_graph` field, doesn't know how to handle this for now..

/// Global context shared across cargo-seamare commands.
#[derive(Debug)]
pub struct CoreContext<'ctx> {
    current_dir: &'ctx Utf8Path,
    package_graph: PackageGraph,
}

impl<'ctx> CoreContext<'ctx> {
    /// Create a new context across the linter.
    ///
    /// It will return error when we can't build guppy `PackageGraph`, such as `current_dir` not inside a rust project.
    pub fn new(current_dir: &Utf8Path) -> Result<CoreContext> {
        let package_graph = Self::build_package_graph(current_dir)?;
        Ok(CoreContext {
            current_dir,
            package_graph,
        })
    }

    pub fn current_dir(&self) -> &Utf8Path {
        &self.current_dir
    }

    pub fn package_graph(&self) -> &PackageGraph {
        &self.package_graph
    }

    pub fn project_root(&self) -> &Utf8Path {
        self.package_graph().workspace().root()
    }

    fn build_package_graph(current_dir: &Utf8Path) -> Result<PackageGraph> {
        let mut cmd = MetadataCommand::new();
        cmd.current_dir(current_dir);
        cmd.build_graph().map_err(|e| SeamareError::Guppy(e))
    }
}
