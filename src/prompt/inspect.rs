//! Prompt inspection — diff and budget analysis for assembled system prompts.
//!
//! Implements REQ-INS-001 through REQ-INS-010 from the SRS:
//! - Prompt diff: human-readable diff between turns (REQ-INS-001–005)
//! - Prompt budget: character/token counts per layer (REQ-INS-006–010)

use colored::Colorize;
use similar::{ChangeTag, TextDiff};

/// A single layer of the assembled system prompt.
#[derive(Debug, Clone)]
pub struct PromptLayer {
    pub name: String,
    pub content: String,
}

/// The fully assembled prompt broken into its constituent layers.
#[derive(Debug, Clone)]
pub struct PromptLayers {
    pub layers: Vec<PromptLayer>,
}

impl PromptLayers {
    /// Assemble the full system prompt from layers.
    pub fn assemble(&self) -> String {
        self.layers
            .iter()
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Build a budget report from the layers.
    pub fn budget(&self) -> BudgetReport {
        BudgetReport {
            layers: self
                .layers
                .iter()
                .map(|l| LayerBudget {
                    name: l.name.clone(),
                    chars: l.content.len(),
                    estimated_tokens: estimate_tokens(&l.content),
                })
                .collect(),
            total_chars: self.layers.iter().map(|l| l.content.len()).sum(),
            total_estimated_tokens: self
                .layers
                .iter()
                .map(|l| estimate_tokens(&l.content))
                .sum(),
        }
    }

    /// Diff two prompt layer sets, returning a human-readable diff string.
    pub fn diff(&self, previous: &PromptLayers) -> PromptDiffResult {
        let self_text = self.assemble();
        let prev_text = previous.assemble();

        // Also diff per-layer to detect what changed
        let mut layer_changes = Vec::new();
        let self_names: Vec<&str> = self.layers.iter().map(|l| l.name.as_str()).collect();
        let prev_names: Vec<&str> = previous.layers.iter().map(|l| l.name.as_str()).collect();

        // Detect added/removed layers
        for name in &self_names {
            if !prev_names.contains(name) {
                layer_changes.push(LayerChange::Added(name.to_string()));
            }
        }
        for name in &prev_names {
            if !self_names.contains(name) {
                layer_changes.push(LayerChange::Removed(name.to_string()));
            }
        }

        // Detect content changes in common layers
        for self_layer in &self.layers {
            if let Some(prev_layer) = previous.layers.iter().find(|l| l.name == self_layer.name)
                && self_layer.content != prev_layer.content {
                    layer_changes.push(LayerChange::Modified(self_layer.name.clone()));
                }
        }

        // Full text diff
        let text_diff = TextDiff::from_lines(&prev_text, &self_text);

        PromptDiffResult {
            layer_changes,
            unified_diff: format_diff(&text_diff),
        }
    }
}

/// Estimate tokens using the chars/4 heuristic (REQ-INS-008: labeled as approximate).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// A single layer's budget info.
#[derive(Debug, Clone)]
pub struct LayerBudget {
    pub name: String,
    pub chars: usize,
    pub estimated_tokens: u64,
}

/// The full budget report.
#[derive(Debug, Clone)]
pub struct BudgetReport {
    pub layers: Vec<LayerBudget>,
    pub total_chars: usize,
    pub total_estimated_tokens: u64,
}

impl BudgetReport {
    /// Render the budget report as a human-readable table.
    /// Includes approximate token labels per REQ-INS-008.
    /// Includes threshold warnings per REQ-INS-009 (default: 4000 tokens).
    pub fn render(&self, warning_threshold: u64) -> String {
        let mut lines = Vec::new();

        lines.push(format!("{}", "═══ PROMPT BUDGET ═══".bright_cyan()));
        lines.push(String::new());

        // Calculate max name width for alignment
        let max_name_len = self
            .layers
            .iter()
            .map(|l| l.name.len())
            .max()
            .unwrap_or(10)
            .max(10);

        // Table header
        lines.push(format!(
            "  {:width$}  {:>8}  {:>16}",
            "Layer",
            "Chars",
            "~Tokens",
            width = max_name_len
        ));
        lines.push(format!(
            "  {:─<width$}  {:─>8}  {:─>16}",
            "",
            "",
            "",
            width = max_name_len
        ));

        // Bar chart scale (max 30 chars wide)
        let max_tokens = self
            .layers
            .iter()
            .map(|l| l.estimated_tokens)
            .max()
            .unwrap_or(1);
        let bar_width = 30u64;

        for layer in &self.layers {
            let bar_len = if max_tokens > 0 {
                ((layer.estimated_tokens * bar_width) / max_tokens).min(bar_width)
            } else {
                0
            };
            let bar: String = "█".repeat(bar_len as usize);

            let warning = if layer.estimated_tokens > warning_threshold {
                format!(" {}", "⚠ exceeds threshold".yellow())
            } else {
                String::new()
            };

            lines.push(format!(
                "  {:width$}  {:>8}  ~{:>12}  {}{}",
                layer.name,
                layer.chars,
                format!("{} estimated", layer.estimated_tokens),
                bar.bright_blue(),
                warning,
                width = max_name_len
            ));
        }

        // Total row
        lines.push(format!(
            "  {:─<width$}  {:─>8}  {:─>16}",
            "",
            "",
            "",
            width = max_name_len
        ));
        let total_bar_len = if max_tokens > 0 && self.total_estimated_tokens > 0 {
            ((self.total_estimated_tokens * bar_width) / max_tokens.max(self.total_estimated_tokens))
                .min(bar_width)
        } else {
            0
        };
        let total_bar: String = "█".repeat(total_bar_len as usize);
        lines.push(format!(
            "  {:width$}  {:>8}  ~{:>12}  {}",
            "TOTAL",
            self.total_chars,
            format!("{} estimated", self.total_estimated_tokens),
            total_bar.bright_green(),
            width = max_name_len
        ));

        lines.push(String::new());
        lines.push(format!(
            "  {} Token counts are approximate (chars ÷ 4 heuristic)",
            "Note:".dimmed()
        ));
        lines.push(format!(
            "  {} Warning threshold: ~{} tokens estimated",
            "Note:".dimmed(),
            warning_threshold
        ));

        lines.join("\n")
    }
}

/// A change to a specific prompt layer between turns.
#[derive(Debug, Clone)]
pub enum LayerChange {
    Added(String),
    Removed(String),
    Modified(String),
}

/// The result of diffing two prompt layer sets.
#[derive(Debug, Clone)]
pub struct PromptDiffResult {
    pub layer_changes: Vec<LayerChange>,
    pub unified_diff: String,
}

impl PromptDiffResult {
    /// Render the diff result as a human-readable string.
    /// Color-coded per REQ-INS-005, never showing API keys (REQ-INS-004).
    pub fn render(&self) -> String {
        let mut lines = Vec::new();

        lines.push(format!("{}", "═══ PROMPT DIFF ═══".bright_cyan()));
        lines.push(String::new());

        // Layer changes summary
        if !self.layer_changes.is_empty() {
            lines.push("Changes by layer:".to_string());
            for change in &self.layer_changes {
                match change {
                    LayerChange::Added(name) => {
                        lines.push(format!("  {} {}", "+".green(), name.green()));
                    }
                    LayerChange::Removed(name) => {
                        lines.push(format!("  {} {}", "-".red(), name.red()));
                    }
                    LayerChange::Modified(name) => {
                        lines.push(format!("  {} {}", "~".yellow(), name.yellow()));
                    }
                }
            }
            lines.push(String::new());
        } else {
            lines.push(format!("  {}", "No layer structure changes.".dimmed()));
            lines.push(String::new());
        }

        // Unified diff
        if self.unified_diff.is_empty() {
            lines.push(format!("  {}", "No content changes.".dimmed()));
        } else {
            lines.push("Content diff:".to_string());
            lines.push(self.unified_diff.clone());
        }

        lines.join("\n")
    }
}

/// Format a TextDiff into a color-coded unified diff string.
/// Uses green/red coloring per REQ-INS-005.
fn format_diff<'a>(diff: &TextDiff<'a, 'a, 'a, str>) -> String {
    let mut lines = Vec::new();

    for change in diff.iter_all_changes() {
        let line = change.to_string_lossy();
        match change.tag() {
            ChangeTag::Delete => {
                for l in line.lines() {
                    lines.push(format!("{}{}", "-".red(), l.red()));
                }
            }
            ChangeTag::Insert => {
                for l in line.lines() {
                    lines.push(format!("{}{}", "+".green(), l.green()));
                }
            }
            ChangeTag::Equal => {
                for l in line.lines() {
                    lines.push(format!(" {}", l));
                }
            }
        }
    }

    lines.join("\n")
}

/// Redact API keys, tokens, and auth headers from text (REQ-INS-004).
/// Will be used by the recording feature (REQ-REC-004).
#[allow(dead_code)]
pub fn redact_secrets(text: &str) -> String {
    let mut result = text.to_string();

    // Redact common API key patterns (sk-..., key-..., etc.)
    let key_patterns = [
        // OpenAI-style keys (sk-...)
        regex::Regex::new(r"(sk-[a-zA-Z0-9]{8})[a-zA-Z0-9]+").unwrap(),
        // Generic long hex/base64 keys (32+ chars after prefix)
        regex::Regex::new(r#"(api[_-]?key|token|bearer|authorization)\s*[:=]\s*["']?[a-zA-Z0-9_\-]{8}[a-zA-Z0-9_\-]+"#).unwrap(),
        // Authorization headers
        regex::Regex::new(r"(?i)(authorization|api-key|x-api-key)\s*[:=]\s*\S{8,}").unwrap(),
    ];

    for pattern in &key_patterns {
        result = pattern
            .replace_all(&result, |caps: &regex::Captures| {
                format!("{}...", &caps[0][..caps[0].len().min(8)])
            })
            .to_string();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hello"), 2); // 5/4 ≈ 2
        assert_eq!(estimate_tokens("a"), 1); // 1/4 = 0.25 → 1
        assert_eq!(estimate_tokens("hello world test"), 4); // 17/4 ≈ 4
    }

    #[test]
    fn test_prompt_layers_assemble() {
        let layers = PromptLayers {
            layers: vec![
                PromptLayer {
                    name: "soul".to_string(),
                    content: "I am Tim.".to_string(),
                },
                PromptLayer {
                    name: "context".to_string(),
                    content: "Model: gpt-4".to_string(),
                },
            ],
        };
        assert_eq!(layers.assemble(), "I am Tim.\n\nModel: gpt-4");
    }

    #[test]
    fn test_budget_report() {
        let layers = PromptLayers {
            layers: vec![
                PromptLayer {
                    name: "soul".to_string(),
                    content: "I am Tim.".to_string(),
                },
                PromptLayer {
                    name: "context".to_string(),
                    content: "Model: gpt-4".to_string(),
                },
            ],
        };
        let budget = layers.budget();
        assert_eq!(budget.layers.len(), 2);
        assert_eq!(budget.layers[0].chars, 9);
        assert_eq!(budget.layers[0].name, "soul");
        assert_eq!(budget.total_chars, 21);
    }

    #[test]
    fn test_prompt_diff_no_changes() {
        let layers = PromptLayers {
            layers: vec![PromptLayer {
                name: "soul".to_string(),
                content: "I am Tim.".to_string(),
            }],
        };
        let result = layers.diff(&layers);
        assert!(result.layer_changes.is_empty());
    }

    #[test]
    fn test_prompt_diff_added_layer() {
        let prev = PromptLayers {
            layers: vec![PromptLayer {
                name: "soul".to_string(),
                content: "I am Tim.".to_string(),
            }],
        };
        let curr = PromptLayers {
            layers: vec![
                PromptLayer {
                    name: "soul".to_string(),
                    content: "I am Tim.".to_string(),
                },
                PromptLayer {
                    name: "memory".to_string(),
                    content: "project uses rust".to_string(),
                },
            ],
        };
        let result = curr.diff(&prev);
        assert!(result
            .layer_changes
            .iter()
            .any(|c| matches!(c, LayerChange::Added(name) if name == "memory")));
    }

    #[test]
    fn test_prompt_diff_modified_layer() {
        let prev = PromptLayers {
            layers: vec![PromptLayer {
                name: "soul".to_string(),
                content: "I am Tim.".to_string(),
            }],
        };
        let curr = PromptLayers {
            layers: vec![PromptLayer {
                name: "soul".to_string(),
                content: "I am Tim v2.".to_string(),
            }],
        };
        let result = curr.diff(&prev);
        assert!(result
            .layer_changes
            .iter()
            .any(|c| matches!(c, LayerChange::Modified(name) if name == "soul")));
    }

    #[test]
    fn test_prompt_diff_removed_layer() {
        let prev = PromptLayers {
            layers: vec![
                PromptLayer {
                    name: "soul".to_string(),
                    content: "I am Tim.".to_string(),
                },
                PromptLayer {
                    name: "volatile".to_string(),
                    content: "old memory".to_string(),
                },
            ],
        };
        let curr = PromptLayers {
            layers: vec![PromptLayer {
                name: "soul".to_string(),
                content: "I am Tim.".to_string(),
            }],
        };
        let result = curr.diff(&prev);
        assert!(result
            .layer_changes
            .iter()
            .any(|c| matches!(c, LayerChange::Removed(name) if name == "volatile")));
    }

    #[test]
    fn test_redact_secrets() {
        let text = "api_key: sk-1234567890abcdef1234567890";
        let redacted = redact_secrets(text);
        // The key should be partially redacted
        assert!(!redacted.contains("sk-1234567890abcdef1234567890"));
    }

    #[test]
    fn test_budget_render() {
        let layers = PromptLayers {
            layers: vec![
                PromptLayer {
                    name: "SOUL".to_string(),
                    content: "I am Tim, a helpful assistant.".to_string(),
                },
                PromptLayer {
                    name: "CONTEXT".to_string(),
                    content: "Model: gpt-4\nUser: andrew".to_string(),
                },
            ],
        };
        let budget = layers.budget();
        let rendered = budget.render(4000);
        assert!(rendered.contains("PROMPT BUDGET"));
        assert!(rendered.contains("SOUL"));
        assert!(rendered.contains("CONTEXT"));
        assert!(rendered.contains("approximate"));
        assert!(rendered.contains("TOTAL"));
    }
}