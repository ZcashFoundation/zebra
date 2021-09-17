use seamare::prelude::*;

/// Configuration for the lint engine.
#[derive(Clone, Debug)]
pub struct LintEngineConfig<'cfg> {
    core: &'cfg CoreContext<'cfg>,
    project_linters: &'cfg [&'cfg dyn ProjectLinter],
    package_linters: &'cfg [&'cfg dyn PackageLinter],
    file_path_linters: &'cfg [&'cfg dyn FilePathLinter],
    content_linters: &'cfg [&'cfg dyn ContentLinter],
    fail_fast: bool,
}

impl<'cfg> LintEngineConfig<'cfg> {
    pub fn new(core: &'cfg CoreContext) -> Self {
        Self {
            core,
            project_linters: &[],
            package_linters: &[],
            file_path_linters: &[],
            content_linters: &[],
            fail_fast: false,
        }
    }

    pub fn with_project_linters(
        &mut self,
        project_linters: &'cfg [&'cfg dyn ProjectLinter],
    ) -> &mut Self {
        self.project_linters = project_linters;
        self
    }

    #[allow(dead_code)]
    pub fn with_package_linters(
        &mut self,
        package_linters: &'cfg [&'cfg dyn PackageLinter],
    ) -> &mut Self {
        self.package_linters = package_linters;
        self
    }

    #[allow(dead_code)]
    pub fn with_file_path_linters(
        &mut self,
        file_path_linters: &'cfg [&'cfg dyn FilePathLinter],
    ) -> &mut Self {
        self.file_path_linters = file_path_linters;
        self
    }

    #[allow(dead_code)]
    pub fn with_content_linters(
        &mut self,
        content_linters: &'cfg [&'cfg dyn ContentLinter],
    ) -> &mut Self {
        self.content_linters = content_linters;
        self
    }

    #[allow(dead_code)]
    pub fn fail_fast(&mut self, fail_fast: bool) -> &mut Self {
        self.fail_fast = fail_fast;
        self
    }

    pub fn build(&self) -> LintEngine<'cfg> {
        LintEngine::new(self.clone())
    }
}

pub struct LintEngine<'cfg> {
    config: LintEngineConfig<'cfg>,
    project_ctx: ProjectContext<'cfg>,
}

impl<'cfg> LintEngine<'cfg> {
    pub fn new(config: LintEngineConfig<'cfg>) -> Self {
        let project_ctx = ProjectContext::new(config.core);
        Self {
            config,
            project_ctx,
        }
    }

    pub fn run(&self) -> Result<LintResults> {
        let mut skipped = vec![];
        let mut messages = vec![];

        // Just run project linters.
        if !self.config.project_linters.is_empty() {
            for linter in self.config.project_linters {
                let source = self.project_ctx.source(linter.name());
                let mut formatter = LintFormatter::new(source, &mut messages);
                match linter.run(&self.project_ctx, &mut formatter)? {
                    RunStatus::Executed => {
                        // Lint ran successfully.
                    }
                    RunStatus::Skipped(reason) => {
                        skipped.push((source, reason));
                    }
                }

                if self.config.fail_fast && !messages.is_empty() {
                    // At least one issue was found.
                    return Ok(LintResults { skipped, messages });
                }
            }
        }
        Ok(LintResults { skipped, messages })
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub struct LintResults<'l> {
    pub skipped: Vec<(LintSource<'l>, SkipReason<'l>)>,
    pub messages: Vec<(LintSource<'l>, LintMessage)>,
}
