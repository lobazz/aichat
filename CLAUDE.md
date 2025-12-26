# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```sh
# Development build
cargo build

# Release build
cargo build --release

# Run tests
cargo test
```

## Architecture Overview

AIChat is an all-in-one LLM CLI tool with a modular architecture.

### Core Modules

- **main.rs**: Entry point. Determines `WorkingMode` (Serve/Repl/Cmd) and orchestrates execution
- **cli.rs**: CLI argument parsing using `clap`
- **config/**: Configuration management
  - `mod.rs`: Central `Config` struct with `GlobalConfig = Arc<RwLock<Config>>`
  - `input.rs`: `Input` struct wraps user prompts with metadata (role, files, etc)
  - `role.rs`, `session.rs`, `agent.rs`, `rag.rs`: Feature-specific state
- **client/**: LLM provider integrations
  - `mod.rs`: Client traits and factory functions
  - `model.rs`: `Model` struct with provider-specific handling
  - Provider clients: `openai.rs`, `claude.rs`, `gemini.rs`, etc.
  - `common.rs`: `RequestData`, `ChatCompletionsData`, request patching via `patch` config
- **repl/**: Interactive REPL implementation using `reedline`
- **serve.rs**: HTTP server for API (OpenAI-compatible) and web UI (Playground/Arena)
- **rag/**: Retrieval-Augmented Generation with embeddings
- **function.rs**: Function calling/tool system

### Key Patterns

- **Config access**: Use `config.read()` for reads, `config.write()` for writes on `GlobalConfig`
- **Request patching**: Models support `patch.url`, `patch.body`, `patch.headers` in config
- **Client trait**: `Client` trait defines `prepare_chat_completions`, `chat_completions`, etc.
- **StateFlags**: Bitflags for REPL command availability (ROLE, SESSION, RAG, AGENT, VS)

### Working Modes

1. **Cmd**: Single prompt execution (`aichat "hello"`)
2. **Repl**: Interactive chat with history, completions, commands (`.help`, `.session`, etc.)
3. **Serve**: HTTP server mode (`--serve`)

### REPL Commands

Commands like `.model`, `.role`, `.session`, `.rag`, `.agent` toggle state flags. See `REPL_COMMANDS` array in `repl/mod.rs` for all commands and their `AssertState` requirements.

## Configuration

Config file: `~/.config/aichat/config.yaml`

```yaml
model: openai:gpt-4o
clients:
  - type: openai
    api_key: ${OPENAI_API_KEY}
    # Supports patch for URL, body, headers
```

Environment variables: `*_api_key`, `AICHAT_LOG_LEVEL`, `AICHAT_CONFIG_FILE`

## Request Patching System

The patch system allows modifying API requests at multiple levels without changing code. Patches use `json_patch::merge()` for deep merging.

### Patch Structure

```yaml
RequestPatch {
  chat_completions: Option<ApiPatch>,  # For chat completions API
  embeddings: Option<ApiPatch>,         # For embeddings API
  rerank: Option<ApiPatch>,             # For rerank API
}

ApiPatch = {
  '<model-regex>': {                    # Regex to match model names
    url?: string,                       # Override request URL
    body?: any,                         # Merge into request body
    headers?: { [key: string]: string } # Add/override headers
  }
}
```

### 1. Client-Level Patch

Applies to ALL models in a client configuration.

```yaml
clients:
  - type: gemini
    api_key: xxx
    patch:
      chat_completions:
        '.*':                                    # Matches all models
          body:
            safetySettings:                     # Added to ALL gemini requests
              - category: HARM_CATEGORY_HARASSMENT
                threshold: BLOCK_NONE
          headers:
            X-Custom-Header: value              # Added to all requests
          url: 'https://proxy.example.com/v1'   # Override URL (rarely used)
```

**Use cases:**
- Add authentication headers for all models
- Set safety settings for all requests
- Use a proxy URL

### 2. Model-Level Patch

Applies to a specific model. Merges ON TOP of client-level patch.

```yaml
clients:
  - type: openai-compatible
    name: ollama
    api_base: http://localhost:11434/v1

    # Client-level: all models
    patch:
      chat_completions:
        '.*':
          headers:
            X-Client-Id: my-client

    models:
      - name: llama3.1
        # Model-level: specific to llama3.1
        patch:
          body:
            temperature: 0.7
            top_p: 0.9
            # Merges with client-level patch

      - name: deepseek-r1
        patch:
          body:
            temperature: 0.3
            max_tokens: 8192
```

**How patches merge:**
```
Client Patch (base)
    ├─ body.safetySettings = [...]
    └─ headers.X-Client-Id = "my-client"
                +
Model Patch (overlay)
    ├─ body.temperature = 0.7         # Added
    └─ body.top_p = 0.9               # Added
                =
Final Request
    ├─ body.safetySettings = [...]
    ├─ body.temperature = 0.7
    ├─ body.top_p = 0.9
    └─ headers.X-Client-Id = "my-client"
```

### 3. Environment Variable Override

Override patches via environment variables (useful for quick changes without config edits).

```bash
# Format: AICHAT_PATCH_<CLIENT_TYPE>_<API_TYPE>
# - Client type: openai, gemini, claude, etc.
# - API type: chat_completions, embeddings, rerank
# - Dashes replaced with underscores, all uppercase

export AICHAT_PATCH_OPENAI_CHAT_COMPLETIONS='{"body":{"temperature":0.1}}'
export AICHAT_PATCH_GEMINI_CHAT_COMPLETIONS='{"headers":{"X-Proxy":"value"}}'
export AICHAT_PATCH_VERTEXAI_EMBEDDINGS='{"body":{"task_type":"retrieval_document}}'
```

### Priority Order (Highest to Lowest)

1. CLI arguments (`--temperature`, etc.)
2. Model-level patch (`model.patch`)
3. **Either** Environment variable **OR** Client-level patch (env var wins if both set)
4. Built-in defaults

**Important: Client-level patches are NOT automatic for all models!** They use regex matching:

```yaml
clients:
  - type: openai
    patch:
      chat_completions:
        'gpt-4o.*':              # Only matches gpt-4o models
          body:
            temperature: 0.5
        '.*':                    # Fallback for non-matching models
          body:
            temperature: 0.7
```

The code applies **only the first matching regex pattern** and then stops.

### Key Data Structures

```rust
// From src/client/common.rs

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RequestPatch {
    pub chat_completions: Option<ApiPatch>,
    pub embeddings: Option<ApiPatch>,
    pub rerank: Option<ApiPatch>,
}

pub type ApiPatch = IndexMap<String, Value>;  // Key = regex pattern, Value = patch

pub struct RequestData {
    pub url: String,
    pub headers: IndexMap<String, String>,
    pub body: Value,
}

impl RequestData {
    pub fn apply_patch(&mut self, patch: Value) {
        // 1. Patch URL if provided
        if let Some(patch_url) = patch["url"].as_str() {
            self.url = patch_url.into();
        }
        // 2. Deep merge body
        if let Some(patch_body) = patch.get("body") {
            json_patch::merge(&mut self.body, patch_body)
        }
        // 3. Update headers
        if let Some(patch_headers) = patch["headers"].as_object() {
            for (key, value) in patch_headers {
                if let Some(value) = value.as_str() {
                    self.header(key, value)
                } else if value.is_null() {
                    self.headers.swap_remove(key);
                }
            }
        }
    }
}

// From src/client/model.rs

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelData {
    pub name: String,
    pub model_type: String,                    // "chat", "embedding", "reranker"
    pub real_name: Option<String>,
    pub max_input_tokens: Option<usize>,
    pub max_output_tokens: Option<usize>,
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    pub supports_vision: bool,
    pub supports_function_calling: bool,
    pub patch: Option<Value>,                  // Model-level patch
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<usize>,
    pub no_stream: bool,
    pub no_system_message: bool,
    pub system_prompt_prefix: Option<String>,
    // ... embedding-only fields
}
```

### Complete Config Example

```yaml
clients:
  # OpenAI with client-level patch
  - type: openai
    api_key: ${OPENAI_API_KEY}
    patch:
      chat_completions:
        'gpt-4o.*':
          body:
            # Override or add any OpenAI body fields
            temperature: 0.5
            response_format:
              type: json_object
        '.*':
          headers:
            X-Organization: my-org

  # Gemini with safety settings and per-model tuning
  - type: gemini
    api_key: ${GEMINI_API_KEY}
    patch:
      chat_completions:
        '.*':
          body:
            safetySettings:
              - category: HARM_CATEGORY_HARASSMENT
                threshold: BLOCK_NONE
              - category: HARM_CATEGORY_HATE_SPEECH
                threshold: BLOCK_NONE
    models:
      - name: gemini-1.5-flash
        patch:
          body:
            generationConfig:
              temperature: 0.1
              maxOutputTokens: 2048
      - name: gemini-1.5-pro
        patch:
          body:
            generationConfig:
              temperature: 0.5
              maxOutputTokens: 8192
              thinkingConfig:
                includeThoughts: true

  # Ollama (openai-compatible) for local models
  - type: openai-compatible
    name: ollama
    api_base: http://localhost:11434/v1
    models:
      - name: llama3.1
        patch:
          body:
            temperature: 0.7
        max_input_tokens: 128000
        supports_function_calling: true
```

### How Patch is Applied

```rust
// From src/client/common.rs - Client::patch_request_data()

fn patch_request_data(&self, request_data: &mut RequestData) {
    let model_type = self.model().model_type();

    // Step 1: Model-level patch (always applied, highest priority)
    if let Some(patch) = self.model().patch() {
        request_data.apply_patch(patch.clone());
    }

    // Step 2: Get either env var OR client patch (env wins if both set)
    let patch_map = std::env::var(get_env_name(&format!(
        "patch_{}_{}",
        self.model().client_name(),
        model_type.api_name(),
    )))
    .ok()
    .and_then(|v| serde_json::from_str(&v).ok())
    .or_else(|| {
        self.patch_config()
            .and_then(|v| model_type.extract_patch(v))
            .cloned()
    });

    // Step 3: Apply first matching regex pattern (stops at first match)
    for (key, patch) in patch_map {
        let key = ESCAPE_SLASH_RE.replace_all(&key, r"\/");
        if let Ok(regex) = Regex::new(&format!("^({key})$")) {
            if let Ok(true) = regex.is_match(self.model().name()) {
                request_data.apply_patch(patch);
                return;  // Stop after first match
            }
        }
    }
}
```

**Key observations:**
1. Model-level patch is applied FIRST and always
2. For client-level, only the FIRST matching regex pattern is applied
3. Forward slashes (`/`) in regex patterns are escaped (`\/`) before matching

### Common Use Cases

| Use Case | Patch Level | Example |
|----------|-------------|---------|
| Add proxy headers | Client | `headers: { Authorization: "Bearer token" }` |
| Set temperature per model | Model | `body: { temperature: 0.3 }` |
| Safety settings (Gemini) | Client | `body: { safetySettings: [...] }` |
| JSON mode (OpenAI) | Model | `body: { response_format: { type: "json_object" } }` |
| Thinking config (Gemini) | Model | `body: { thinkingConfig: { includeThoughts: true } }` |
| Quick override without config | Env | `AICHAT_PATCH_OPENAI_CHAT_COMPLETIONS='{"body":{"temperature":0}}'` |
| Use max_completion_tokens (OpenAI) | Model | `body: { max_tokens: null }` (triggers use of `max_completion_tokens`) |

### Important Notes

1. **OpenAI `max_tokens` vs `max_completion_tokens`**: Newer OpenAI models use `max_completion_tokens` instead of `max_tokens`. To force this via patch:
   ```yaml
   model:
     name: gpt-4o
     patch:
       body:
         max_tokens: null  # Triggers use of max_completion_tokens
   ```

2. **Client-level patches are NOT cumulative**: Only the **first matching regex pattern** is applied. Later patterns are ignored even if they also match.

3. **Using `null` to remove fields**: You can set values to `null` to remove them from the request:
   ```yaml
   # Example: Disable temperature/top_p for reasoning models (o1, o3, o4)
   model:
     name: o1
     patch:
       body:
         temperature: null
         top_p: null
         max_tokens: null
   ```

4. **Model names with special chars**: Forward slashes in model names (e.g., `deepseek-r1`) are automatically escaped in regex matching.
