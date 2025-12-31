use super::*;

use anyhow::{bail, Result};
use crossterm::terminal;
use std::borrow::Cow;
use std::io::Write;

use crate::config::input::Input;
use crate::utils::AbortSignal;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};

#[derive(Debug, Clone)]
pub struct VsMode {
    pub models: Vec<Model>,
}

#[derive(Debug)]
struct ModelResponse {
    display_index: usize,
    original_index: usize,
    model_id: String,
    result: Result<String, anyhow::Error>,
}

/// Prompt for VS mode selection using Reedline
struct SelectionPrompt {
    text: String,
}

impl SelectionPrompt {
    fn new(max_selection: usize) -> Self {
        Self {
            text: format!("Select response [1-{}] (or 'q' to quit, Ctrl+D to exit): ", max_selection),
        }
    }
}

impl Prompt for SelectionPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.text)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
}

/// Prepare input with the specified model's role
fn prepare_input_with_model(input: &Input, model: &Model) -> Input {
    let mut model_input = input.clone();
    let mut role_with_new_model = model_input.role().clone();
    role_with_new_model.set_model(model.clone());
    model_input.set_role(role_with_new_model);
    model_input
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

enum ParseAction {
    Retry,
    Exit,
    Invalid(String),
    Select(usize),
}

/// Parse user selection from input string
fn parse_selection(input: &str, max_options: usize) -> ParseAction {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return ParseAction::Retry;
    }

    if matches!(trimmed.to_lowercase().as_str(), "exit" | "quit" | "q") {
        return ParseAction::Exit;
    }

    match trimmed.parse::<usize>() {
        Ok(n) if n >= 1 && n <= max_options => ParseAction::Select(n),
        Ok(_) => ParseAction::Invalid(format!("Selection out of range (1-{max_options})")),
        Err(_) => ParseAction::Invalid("Invalid selection".into()),
    }
}

/// Read user selection from stdin (interactive or non-interactive)
fn read_user_selection(interactive: bool, count: usize) -> Result<Option<usize>> {
    use std::io::{stdin, stdout, Write};
    
    if interactive {
        let prompt = SelectionPrompt::new(count);
        let mut editor = Reedline::create();
        loop {
            match editor.read_line(&prompt) {
                Ok(Signal::Success(line)) => {
                    match parse_selection(&line, count) {
                        ParseAction::Select(n) => return Ok(Some(n)),
                        ParseAction::Exit => return Ok(None),
                        ParseAction::Invalid(msg) => eprintln!("Error: {msg}"),
                        ParseAction::Retry => {}
                    }
                }
                Ok(Signal::CtrlC) => {
                    println!("(To exit, press Ctrl+D or enter 'q')");
                }
                Ok(Signal::CtrlD) => return Ok(None),
                _ => {}
            }
        }
    } else {
        loop {
            print!("Select response [1-{count}] (or 'q' to quit, Ctrl+D to exit): ");
            stdout().flush()?;
            let mut input = String::new();
            match stdin().read_line(&mut input) {
                Ok(0) => return Ok(None),
                Ok(_) => {
                    match parse_selection(&input, count) {
                        ParseAction::Select(n) => return Ok(Some(n)),
                        ParseAction::Exit => return Ok(None),
                        ParseAction::Invalid(msg) => eprintln!("Error: {msg}"),
                        ParseAction::Retry => {}
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => return Ok(None),
                Err(e) => bail!("Failed to read input: {e}"),
            }
        }
    }
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
/// selection_config: Some(config) enables interactive mode, None uses non-interactive mode
pub async fn ask_vs(
    config: &GlobalConfig,
    input: Input,
    abort_signal: AbortSignal,
    selection_config: Option<GlobalConfig>,
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
            let result = async {
                let model_input = prepare_input_with_model(&input, &model);
                let client = model_input.create_client()?;
                let output = client.chat_completions(model_input.clone()).await?;
                Ok(output.text)
            }.await;
            let _ = tx.send((index, model.id().to_string(), result));
        });
    }

    drop(tx);

    let mut completed = 0;
    let mut interrupted = false;

    // Use tokio::select! to handle Ctrl+C signal properly
    loop {
        tokio::select! {
            // Receive model responses
            result = rx.recv() => {
                match result {
                Some((_index, model_id, response)) => {
                    completed += 1;
                    print_response_header(completed, &model_id);
                    display_response(config, &response)?;
                    responses.push(ModelResponse {
                        display_index: completed,
                        original_index: _index,
                        model_id,
                        result: response,
                    });

                        // Check if all models responded
                        if completed >= total_models {
                            break;
                        }
                    }
                    None => {
                        // Channel closed - all senders dropped
                        break;
                    }
                }
            }
            // Handle Ctrl+C via tokio signal handler
            _ = tokio::signal::ctrl_c() => {
                abort_signal.set_ctrlc();
                interrupted = true;
                break;
            }
        }
    }

    // If interrupted, return to REPL
    if interrupted {
        eprintln!("\nInterrupted");
        return Ok(());
    }

    // Show selection menu only in REPL mode (interactive)
    if selection_config.is_some() {
        select_response_without_display(config, &input, &responses, selection_config)?;
    }

    Ok(())
}


/// Handle response selection without displaying results (already printed)
fn select_response_without_display(
    config: &GlobalConfig,
    user_input: &Input,
    results: &[ModelResponse],
    selection_config: Option<GlobalConfig>,
) -> Result<()> {
    println!();

    let valid_responses: Vec<&ModelResponse> = results
        .iter()
        .filter(|r| r.result.is_ok())
        .collect();

    if valid_responses.is_empty() {
        bail!("No valid responses to select from");
    }

    for (idx, resp) in valid_responses.iter().enumerate() {
        println!("  [{}] {}", idx + 1, resp.model_id);
    }

    let is_interactive = selection_config.is_some();
    match read_user_selection(is_interactive, valid_responses.len())? {
        Some(index) => {
            let selected = valid_responses.get(index - 1)
                .ok_or_else(|| anyhow::anyhow!("Invalid selection"))?;
            handle_selection(config, user_input, selected)?;
        }
        None => {
            println!("Exiting VS mode selection...");
        }
    }
    Ok(())
}

/// Handle a valid selection from the user
fn handle_selection(
    config: &GlobalConfig,
    user_input: &Input,
    selected: &ModelResponse,
) -> Result<()> {
    let response = selected.result.as_ref().unwrap();

    let selected_model = {
        let cfg = config.read();
        cfg.vs_mode
            .as_ref()
            .and_then(|vs| vs.models.get(selected.original_index).cloned())
            .unwrap_or_else(|| cfg.model.clone())
    };

    let mut cfg = config.write();
    cfg.after_chat_completion(user_input, response, &[])?;

    if let Some(session) = &mut cfg.session {
        session.set_model(selected_model);
    }
    Ok(())
}

impl Config {
    pub fn exit_vs_mode(&mut self) -> Result<()> {
        self.vs_mode = None;
        Ok(())
    }
}
