//! Session summary generation on REPL exit.

use anyhow::Result;

use crate::api::{LlmClient, Message};

/// Generate a concise session summary from the conversation messages.
///
/// Returns `None` if there are too few messages to summarize meaningfully
/// (just the system prompt, or system + 1 user message with no assistant reply).
pub fn should_summarize(messages: &[Message]) -> bool {
    // Need at least system + user + assistant to have a conversation worth summarizing.
    let user_msgs = messages.iter().filter(|m| m.role == "user").count();
    let assistant_msgs = messages.iter().filter(|m| m.role == "assistant").count();
    user_msgs >= 1 && assistant_msgs >= 1
}

/// Build a summary prompt from conversation messages.
///
/// Extracts user and assistant turns, truncates if very long to stay within token limits,
/// and asks the LLM to produce a concise session summary.
fn build_summary_prompt(messages: &[Message]) -> String {
    let mut turns = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "system" | "tool" => continue,
            "user" => {
                if let Some(content) = &msg.content {
                    turns.push(format!("User: {}", content));
                }
            }
            "assistant" => {
                if let Some(content) = &msg.content {
                    // Truncate very long assistant messages
                    let truncated = if content.len() > 500 {
                        format!("{}...[truncated]", &content[..500])
                    } else {
                        content.clone()
                    };
                    turns.push(format!("Assistant: {}", truncated));
                } else if msg.tool_calls.is_some() {
                    turns.push("Assistant: [used tools]".to_string());
                }
            }
            _ => continue,
        }
    }

    // Truncate total conversation if very long
    let conversation = turns.join("\n\n");
    let conversation = if conversation.len() > 8000 {
        &conversation[..8000]
    } else {
        &conversation
    };

    format!(
        "Summarize this session in 3-5 concise bullet points. \
         Focus on: topics discussed, decisions made, work completed, \
         and open tasks or next steps. Be brief and information-dense.\n\n{}",
        conversation
    )
}

/// Generate a session summary by calling the LLM.
///
/// Uses a non-streaming call with a 10-second timeout.
/// Falls back to a minimal summary on timeout or error.
pub async fn generate_session_summary(
    client: &LlmClient,
    messages: &[Message],
) -> Result<String> {
    if !should_summarize(messages) {
        return Ok(String::new());
    }

    let prompt = build_summary_prompt(messages);
    let summary_messages = vec![
        Message::system(
            "You are a session summarizer. Produce concise, factual summaries. \
             Format as bullet points starting with '- '. \
             Focus on what was done and decided, not on meta-conversation."
        ),
        Message::user(&prompt),
    ];

    // Non-streaming call — we don't need to display it
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client.chat(summary_messages, None),
    )
    .await??;

    let summary = result.content.unwrap_or_default();
    Ok(summary)
}

/// Generate a fallback summary when the LLM call fails or times out.
pub fn fallback_summary(messages: &[Message]) -> String {
    let user_count = messages.iter().filter(|m| m.role == "user").count();
    let assistant_count = messages.iter().filter(|m| m.role == "assistant").count();
    format!(
        "Session ended. {} user messages, {} assistant responses.",
        user_count, assistant_count
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_not_summarize_empty() {
        let messages = vec![Message::system("You are helpful.")];
        assert!(!should_summarize(&messages));
    }

    #[test]
    fn should_not_summarize_single_user() {
        let messages = vec![
            Message::system("You are helpful."),
            Message::user("hello"),
        ];
        assert!(!should_summarize(&messages));
    }

    #[test]
    fn should_summarize_conversation() {
        let messages = vec![
            Message::system("You are helpful."),
            Message::user("hello"),
            Message::assistant("hi there"),
        ];
        assert!(should_summarize(&messages));
    }

    #[test]
    fn build_summary_prompt_excludes_system_and_tool() {
        let messages = vec![
            Message::system("You are helpful."),
            Message::user("what is 2+2"),
            Message::assistant_with_tools(
                vec![crate::api::ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: crate::api::ToolCallFunction {
                        name: "exec_command".to_string(),
                        arguments: "{}".to_string(),
                    },
                }],
                None,
            ),
            Message::tool_result("1", "4"),
            Message::assistant("The answer is 4."),
        ];
        let prompt = build_summary_prompt(&messages);
        assert!(prompt.contains("User: what is 2+2"));
        assert!(!prompt.contains("You are helpful")); // system excluded
        assert!(prompt.contains("[used tools]")); // assistant with tool calls
        assert!(prompt.contains("The answer is 4."));
    }

    #[test]
    fn fallback_summary_counts_messages() {
        let messages = vec![
            Message::system("hi"),
            Message::user("hello"),
            Message::assistant("world"),
            Message::user("again"),
        ];
        let summary = fallback_summary(&messages);
        assert!(summary.contains("2 user messages"));
        assert!(summary.contains("1 assistant responses"));
    }
}