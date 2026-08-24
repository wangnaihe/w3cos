//! Feature-neutral host boundary for HTML parser script checkpoints.

use anyhow::Result;

pub(crate) trait ParserScriptHost {
    fn begin_document_parse(&self, document_url: &str) -> Result<()>;
    fn has_pending_parser_blocking_script(&self) -> bool;
    fn perform_microtask_checkpoint(&self);
    fn execute_pending_document_scripts(&self, document_url: &str) -> Result<()>;
    fn finish_parser_style(&self, node: u32, document_url: &str) -> Result<()>;
    fn register_inline_event_handler(&self, node: u32, event_type: &str, source: &str);
    fn finish_document_parse(&self);
}

#[derive(Debug, Default)]
pub(crate) struct InertParserScriptHost;

impl ParserScriptHost for InertParserScriptHost {
    fn begin_document_parse(&self, _document_url: &str) -> Result<()> {
        Ok(())
    }

    fn has_pending_parser_blocking_script(&self) -> bool {
        false
    }

    fn perform_microtask_checkpoint(&self) {}

    fn execute_pending_document_scripts(&self, _document_url: &str) -> Result<()> {
        Ok(())
    }

    fn finish_parser_style(&self, _node: u32, _document_url: &str) -> Result<()> {
        Ok(())
    }

    fn register_inline_event_handler(&self, _node: u32, _event_type: &str, _source: &str) {}

    fn finish_document_parse(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_host_never_blocks_or_executes_another_engine() {
        let host = InertParserScriptHost;
        host.begin_document_parse("about:blank").unwrap();
        host.execute_pending_document_scripts("about:blank")
            .unwrap();
        host.perform_microtask_checkpoint();
        assert!(!host.has_pending_parser_blocking_script());
        host.finish_document_parse();
    }
}
