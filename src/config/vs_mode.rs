use super::*;

use anyhow::{bail, Result};
use crossterm::terminal;
use std::io::Write;

use crate::config::input::Input;

#[derive(Debug, Clone)]
pub struct VsMode {
    pub models: Vec<Model>,
}

type VsResponse = (usize, String, Result<String, anyhow::Error>);

/// Prepare input with the specified model's role
fn prepare_input_with_model(input: &Input, model: &Model) -> Input {
    let mut model_input = input.clone();
    let mut role_with_new_model = model_input.role().clone();
    role_with_new_model.set_model(model.clone());
    model_input.set_role(role_with_new_model);
    model_input
}

/// Query a single model and return the text response
async fn query_model(input: Input, model: Model) -> Result<String> {
    let model_input = prepare_input_with_model(&input, &model);
    let client = model_input.create_client()?;
    let output = client.chat_completions(model_input.clone()).await?;
    Ok(output.text)
}

/// Print a model response header with terminal-width dashes
fn print_response_header(index: usize, model_id: &str) {
    println!();
    let header = format!("--- [{}] {} ---", index, model_id);
    let width = terminal::size().map(|(w, _)| w).unwrap_or(80) as usize;
    let dash_count = width.saturating_sub(header.len());
    println!("{}{}", header, "-".repeat(dash_count));
}

/// Display a single model response or error
fn display_response(config: &GlobalConfig, result: &Result<String, anyhow::Error>) -> Result<()> {
    match result {
        Ok(output) => {
            config.read().print_markdown(output)?;
        }
        Err(e) => {
            if let Some(source) = e.source() {
                eprintln!("Error: {}: {}", e, source);
            } else {
                eprintln!("Error: {}", e);
            }
        }
    }
    Ok(())
}

/// Parse user selection from input string
fn parse_selection(input: &str, max_value: usize) -> Result<usize> {
    let trimmed = input.trim();

    if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
        std::process::exit(0);
    }

    let selection: usize = trimmed
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid selection"))?;

    if selection < 1 || selection > max_value {
        bail!("Invalid selection");
    }

    Ok(selection)
}

/// Initialize VS mode with the specified models
pub async fn vs_mode_init(
    config: &GlobalConfig,
    models_str: &str,
) -> Result<()> {
    let models_list: Vec<&str> = models_str.split(',').map(|s| s.trim()).collect();

    if models_list.len() < 2 {
        bail!("VS mode requires at least 2 models");
    }

    let mut models = Vec::new();
    for model_id in &models_list {
        let model = Model::retrieve_model(&config.read(), model_id, crate::ModelType::Chat)?;
        models.push(model);
    }

    let vs_mode = VsMode {
        models,
    };

    config.write().vs_mode = Some(vs_mode);
    println!("VS mode initialized with {} models", models_list.len());

    Ok(())
}

/// Query all VS mode models with the given input and display results
/// show_selection: true for REPL (interactive), false for non-interactive
pub async fn ask_vs(
    config: &GlobalConfig,
    input: Input,
    _abort_signal: AbortSignal,
    show_selection: bool,
) -> Result<()> {
    // Don't send empty messages (same as regular REPL)
    if input.is_empty() {
        return Ok(());
    }

    let vs_mode = {
        let cfg = config.read();
        cfg.vs_mode.as_ref().cloned()
    };

    let Some(vs_mode) = vs_mode else {
        bail!("Not in VS mode");
    };

    let total_models = vs_mode.models.len();
    let mut responses = Vec::with_capacity(total_models);

    // Use synchronous "Generating..." text instead of async spinner
    // This avoids race conditions with terminal cursor/clear
    print!("Generating... ");
    std::io::stdout().flush()?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    for (index, model) in vs_mode.models.iter().enumerate() {
        let model = model.clone();
        let input = input.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let result = query_model(input, model.clone()).await;
            let _ = tx.send((index, model.id().to_string(), result));
        });
    }

    drop(tx);

    let mut completed = 0;
    while let Some((_index, model_id, result)) = rx.recv().await {
        completed += 1;
        print_response_header(completed, &model_id);
        display_response(config, &result)?;
        responses.push((completed, model_id, result));
    }

    // Show selection menu only in REPL mode (interactive)
    if show_selection {
        select_response_without_display(config, &input, &responses)?;
    }

    Ok(())
}


/// Handle response selection without displaying results (already printed)
fn select_response_without_display(
    config: &GlobalConfig,
    user_input: &Input,
    results: &[VsResponse],
) -> Result<()> {
    println!();

    let mut display_order = Vec::new();
    for (display_index, model_id, result) in results {
        if result.is_ok() {
            display_order.push((display_index, model_id));
            println!("  [{}] {}", display_index, model_id);
        }
    }

    if display_order.is_empty() {
        bail!("No valid responses to select from");
    }

    print!("Select response [1-{}] (or 'exit' to quit): ", display_order.len());
    std::io::stdout().flush()?;

    let mut selection_str = String::new();
    std::io::stdin().read_line(&mut selection_str)?;
    let selection = parse_selection(&selection_str, display_order.len())?;
    let selected_display_index = display_order[selection - 1].0;

    // Find the result by display index
    let (_, _, result) = results.iter()
        .find(|(idx, _, _)| *idx == *selected_display_index)
        .ok_or_else(|| anyhow::anyhow!("Selected response not found"))?;

    if let Ok(response) = result {
        // Get the selected model
        let selected_model = {
            let cfg = config.read();
            let vs_mode = cfg.vs_mode.as_ref().unwrap();

            // Find the original model index by matching the display index
            let original_index = results.iter()
                .position(|(idx, _, _)| *idx == *selected_display_index)
                .unwrap_or(0);

            vs_mode.models
                .get(original_index)
                .cloned()
                .unwrap_or_else(|| cfg.model.clone())
        };

        // Add both user prompt and selected response to conversation history
        let mut cfg = config.write();
        cfg.after_chat_completion(user_input, response.as_str(), &[])?;

        // Update the session's model to the selected one
        if let Some(session) = &mut cfg.session {
            session.set_model(selected_model);
        }
    }

    Ok(())
}

impl Config {
    pub fn exit_vs_mode(&mut self) -> Result<()> {
        self.vs_mode = None;
        Ok(())
    }
}
