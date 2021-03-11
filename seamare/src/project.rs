// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{prelude::*, LintContext};
use guppy::graph::PackageGraph;
use std::path::{Path, PathBuf};

/// Represents a linter that checks some property for the overall project.
///
/// Linters that implement `ProjectLinter` will run once for the whole project.
pub trait ProjectLinter: Linter {
    // Since ProjectContext is only 1 word long, clippy complains about passing it by reference. Do
    // it that way for consistency reasons.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    /// Executes the lint against the given project context.
    fn run<'l>(
        &self,
        ctx: &ProjectContext<'l>,
        out: &mut LintFormatter<'l, '_>,
    ) -> Result<RunStatus<'l>>;
}

/// Overall linter context for a project.
#[derive(Debug)]
pub struct ProjectContext<'l> {
    core: &'l CoreContext,
}

impl<'l> ProjectContext<'l> {
    pub fn new(core: &'l CoreContext) -> Self {
        Self { core }
    }

    /// Returns the core context.
    pub fn core(&self) -> &'l CoreContext {
        self.core
    }

    /// Returns the project root.
    pub fn project_root(&self) -> &'l Path {
        self.core.project_root()
    }

    /// Returns the package graph, computing it for the first time if necessary.
    pub fn package_graph(&self) -> Result<&'l PackageGraph> {
        Ok(self.core.package_graph()?)
    }

    /// Returns the absolute path from the project root.
    pub fn full_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.core.project_root().join(path.as_ref())
    }
}

impl<'l> LintContext<'l> for ProjectContext<'l> {
    fn kind(&self) -> LintKind<'l> {
        LintKind::Project
    }
}
