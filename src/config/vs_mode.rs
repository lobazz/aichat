use super::*;

use anyhow::{bail, Result};
use crossterm::terminal;
use std::io::Write;

use crate::config::input::Input;

#[derive(Debug, Clone)]
pub struct VsMode {
    pub models: Vec<Model>,
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
    abort_signal: AbortSignal,
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
        let _abort_signal = abort_signal.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let mut model_input = input.clone();
            let mut role_with_new_model = model_input.role().clone();
            role_with_new_model.set_model(model.clone());
            model_input.set_role(role_with_new_model);

            let result = async {
                let client = model_input.create_client()?;

                // Call chat completions directly WITHOUT internal spinner
                // This avoids terminal cursor races with VS mode's own display
                let output = client.chat_completions(model_input.clone()).await?;

                // Extract just the text from ChatCompletionsOutput
                Ok::<_, anyhow::Error>(output.text)
            }.await;

            let _ = tx.send((index, model.id().to_string(), result));
        });
    }

    drop(tx);

    let mut completed = 0;
    while let Some((_index, model_id, result)) = rx.recv().await {
        completed += 1;

        // Clear the "Generating..." line and print header below it
        println!();
        let header = format!("--- [{}] {} ---", completed, model_id);
        let width = terminal::size().map(|(w, _)| w).unwrap_or(80) as usize;
        let dash_count = width.saturating_sub(header.len());
        println!("{}{}", header, "-".repeat(dash_count));

        match &result {
            Ok(output) => {
                // Print the output manually
                config.read().print_markdown(output)?;
            }
            Err(e) => {
                // Show the real error, not just the context wrapper
                if let Some(source) = e.source() {
                    eprintln!("Error: {}: {}", e, source);
                } else {
                    eprintln!("Error: {}", e);
                }
            }
        }

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
    results: &[(usize, String, Result<String, anyhow::Error>)],
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
    let selection_str = selection_str.trim();

    if selection_str.eq_ignore_ascii_case("exit") || selection_str.eq_ignore_ascii_case("quit") {
        std::process::exit(0);
    }

    let selection: usize = selection_str.parse()
        .map_err(|_| anyhow::anyhow!("Invalid selection"))?;

    if selection < 1 || selection > display_order.len() {
        bail!("Invalid selection");
    }

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
        cfg.after_chat_completion(user_input, &response, &[])?;

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
