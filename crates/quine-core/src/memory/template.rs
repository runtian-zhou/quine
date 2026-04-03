#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummaryDocument {
    pub(crate) current_state: Vec<String>,
    pub(crate) task_specification: Vec<String>,
    pub(crate) files_and_functions: Vec<String>,
    pub(crate) workflow: Vec<String>,
    pub(crate) errors_and_corrections: Vec<String>,
    pub(crate) codebase_and_system_documentation: Vec<String>,
    pub(crate) learnings: Vec<String>,
    pub(crate) key_results: Vec<String>,
    pub(crate) worklog: Vec<String>,
}

impl SessionSummaryDocument {
    pub(crate) fn empty() -> Self {
        Self {
            current_state: vec!["No summary yet.".into()],
            task_specification: vec!["No task specification captured yet.".into()],
            files_and_functions: vec!["No files or functions captured yet.".into()],
            workflow: vec!["No workflow notes captured yet.".into()],
            errors_and_corrections: vec!["No errors or corrections captured yet.".into()],
            codebase_and_system_documentation: vec!["No codebase notes captured yet.".into()],
            learnings: vec!["No learnings captured yet.".into()],
            key_results: vec!["No key results captured yet.".into()],
            worklog: vec!["No worklog entries captured yet.".into()],
        }
    }

    pub(crate) fn render_markdown(&self) -> String {
        let mut output = String::new();
        Self::render_section(&mut output, "Current State", &self.current_state);
        Self::render_section(&mut output, "Task Specification", &self.task_specification);
        Self::render_section(
            &mut output,
            "Files and Functions",
            &self.files_and_functions,
        );
        Self::render_section(&mut output, "Workflow", &self.workflow);
        Self::render_section(
            &mut output,
            "Errors & Corrections",
            &self.errors_and_corrections,
        );
        Self::render_section(
            &mut output,
            "Codebase and System Documentation",
            &self.codebase_and_system_documentation,
        );
        Self::render_section(&mut output, "Learnings", &self.learnings);
        Self::render_section(&mut output, "Key Results", &self.key_results);
        Self::render_section(&mut output, "Worklog", &self.worklog);
        output
    }

    fn render_section(output: &mut String, title: &str, items: &[String]) {
        output.push_str("## ");
        output.push_str(title);
        output.push_str("\n\n");
        for item in items {
            output.push_str("- ");
            output.push_str(item);
            output.push('\n');
        }
        output.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::SessionSummaryDocument;

    #[test]
    fn summary_template_contains_canonical_headings() {
        let markdown = SessionSummaryDocument::empty().render_markdown();
        for heading in [
            "## Current State",
            "## Task Specification",
            "## Files and Functions",
            "## Workflow",
            "## Errors & Corrections",
            "## Codebase and System Documentation",
            "## Learnings",
            "## Key Results",
            "## Worklog",
        ] {
            assert!(markdown.contains(heading), "missing heading: {heading}");
        }
    }
}
