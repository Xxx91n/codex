use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TurnAborted {
    pub(crate) guidance: String,
}

impl TurnAborted {
    pub(crate) const INTERRUPTED_GUIDANCE: &'static str = "The user interrupted the previous turn on purpose. Any running unified exec processes may still be running in the background. If any tools/commands were aborted, they may have partially executed.";
    pub(crate) const INTERRUPTED_DEVELOPER_GUIDANCE: &'static str = "The previous turn was interrupted on purpose. Any running unified exec processes may still be running in the background. If any tools/commands were aborted, they may have partially executed.";
    /// Preemption of a Chat/Messages stream by a new user message (spec S-07).
    /// Role-neutral on purpose: nothing here may suggest the user cancelled the
    /// turn, and no tool may be described as partially executed.
    pub(crate) const PREEMPTED_GUIDANCE: &'static str = "The previous response was interrupted by a new user message before it finished. Text that was fully received has been kept; the last text segment may be incomplete. Unfinished tool calls were not executed. Continue from the new user message.";

    pub(crate) fn new(guidance: impl Into<String>) -> Self {
        Self {
            guidance: guidance.into(),
        }
    }
}

impl ContextualUserFragment for TurnAborted {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("generic.turn_aborted".to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<turn_aborted>", "</turn_aborted>")
    }

    fn body(&self) -> String {
        format!("\n{}\n", self.guidance)
    }
}
