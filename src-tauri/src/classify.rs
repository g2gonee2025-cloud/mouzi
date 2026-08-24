//! AI-assisted file classification module.
//!
//! Zero Tauri dependencies. Provides a heuristic classifier (extension + filename
//! token scoring), co-occurrence learning from `action_logs`, and an optional
//! Ollama provider that falls back to heuristic on any error.
//!
//! The public API is:
//! - `suggest_category(filename, extension) -> &'static str` (pure heuristic)
//! - `learned_hints() -> Vec<LearnedHint>` (co-occurrence from action_logs)
//! - `AiProvider` trait (detect_provider returns one)
//! - `HeuristicProvider` (always available)
//! - `OllamaProvider` (optional, runtime-detected)
//! - `Suggestion` struct with confidence scoring

use crate::db;
use serde::Serialize;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Static extension → category map (mirrors scan.rs::categorize but returns
// Option so the caller can apply filename-token scoring for "Other" files).
// ---------------------------------------------------------------------------

/// Return the category for a given extension, or `None` if unknown.
/// Extensions are matched without the leading dot.
pub fn extension_to_category(ext: &str) -> Option<&'static str> {
    let ext = ext.trim_start_matches('.').to_lowercase();
    match ext.as_str() {
        "pdf" | "doc" | "docx" | "txt" | "md" | "xls" | "xlsx" | "ppt" | "pptx" | "csv"
        | "rtf" | "odt" | "ods" | "odp" => Some("Documents"),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "heic" | "heif" | "bmp" | "ico" => {
            Some("Images")
        }
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" => Some("Videos"),
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" => Some("Audio"),
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "dmg" => Some("Archives"),
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "java" | "c" | "cpp" | "h" | "hpp"
        | "go" | "rb" | "cs" | "swift" | "kt" | "scala" | "php" | "pl" | "sh" | "bash"
        | "css" | "scss" | "less" | "html" | "htm" | "json" | "xml" | "yaml" | "yml"
        | "toml" | "sql" | "vue" | "svelte" | "astro" => Some("Code"),
        "exe" | "msi" | "msix" | "appx" | "deb" | "rpm" | "apk" | "appimage" => {
            Some("Archives")
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Filename token scoring
// ---------------------------------------------------------------------------

/// Score a filename token (uppercased, stem part) against known patterns.
/// Returns a list of (category, confidence_boost) pairs.
fn score_filename_token(token: &str) -> Vec<(&'static str, f64)> {
    let upper = token.to_uppercase();
    let mut scores = Vec::new();

    // Document-related tokens
    let doc_tokens = [
        "INVOICE",
        "RECEIPT",
        "RESUME",
        "CV",
        "COVERLETTER",
        "LETTER",
        "BUDGET",
        "REPORT",
        "STATEMENT",
        "CONTRACT",
        "AGREEMENT",
        "PROPOSAL",
        "QUOTE",
        "ESTIMATE",
        "BILL",
        "TAX",
        "RECORD",
        "FORM",
        "APPLICATION",
        "TRANSCRIPT",
        "CERTIFICATE",
        "DIPLOMA",
        "THESIS",
        "PAPER",
        "ESSAY",
        "NOTE",
        "NOTES",
        "MINUTES",
        "AGENDA",
        "MEMO",
        "FAX",
        "DOC",
        "DOCUMENT",
        "SPREADSHEET",
        "CALC",
    ];
    if doc_tokens.contains(&upper.as_str()) {
        scores.push(("Documents", 0.35));
    }

    // Image-related tokens
    let img_tokens = [
        "IMG",
        "IMG_",
        "DSC",
        "DSC_",
        "DSCN",
        "PICTURE",
        "PHOTO",
        "IMAGE",
        "SCREENSHOT",
        "SCREENSHOTS",
        "SNAPSHOT",
        "CAPTURE",
        "SCREENSHOT_",
        "PIC",
        "SELFIE",
        "BANNER",
        "THUMBNAIL",
        "ARTWORK",
        "IMG_",
        "DSC_",
        "DSCN",
    ];
    // Check prefix and exact match for image tokens
    for t in &img_tokens {
        if upper.as_str() == *t || upper.starts_with(t) {
            scores.push(("Images", 0.35));
            break;
        }
    }

    // Video-related tokens
    let vid_tokens = [
        "VIDEO",
        "VID_",
        "VIDEO_",
        "RECORDING",
        "SCREENRECORDING",
        "SCREEN_RECORDING",
        "CLIP",
        "FOOTAGE",
        "MOVIE",
        "FILM",
        "TUTORIAL",
        "CAMERA",
    ];
    for t in &vid_tokens {
        if upper.as_str() == *t || upper.starts_with(t) {
            scores.push(("Videos", 0.35));
            break;
        }
    }

    // Audio-related tokens
    let audio_tokens = [
        "AUDIO",
        "RECORDING",
        "VOICE",
        "VOICE_",
        "VOICENOTE",
        "PODCAST",
        "SONG",
        "MUSIC",
        "TRACK",
        "BEAT",
        "MIX",
        "REC",
        "REC_",
        "VOICEMEMO",
    ];
    for t in &audio_tokens {
        if upper.as_str() == *t || upper.starts_with(t) {
            scores.push(("Audio", 0.35));
            break;
        }
    }

    // Archive-related tokens
    let archive_tokens = [
        "INSTALL",
        "INSTALLER",
        "SETUP",
        "BOOT",
        "RELEASE",
        "BACKUP",
        "BACKUP_",
        "ARCHIVE",
        "ARCHIVE_",
        "DUMP",
        "EXPORT",
        "EXPORT_",
        "PACKAGE",
        "DIST",
        "BUILD",
        "DOWNLOAD",
    ];
    if archive_tokens.contains(&upper.as_str()) {
        scores.push(("Archives", 0.30));
    }

    // Code-related tokens
    let code_tokens = [
        "SOURCE",
        "SRC",
        "CODE",
        "PROJECT",
        "PROJECT_",
        "REPO",
        "GIT",
        "GITHUB",
        "PACKAGE",
        "MODULE",
        "LIB",
        "LIBRARY",
        "BIN",
        "BINARY",
        "SCRIPT",
        "CONFIG",
        "CONFIG_",
        "CONF",
        "SETTINGS",
        "DOTFILE",
        "DOTFILES",
        "VENDOR",
        "NODE_MODULES",
        "COMPONENT",
        "COMPONENTS",
        "PAGE",
        "PAGES",
    ];
    if code_tokens.contains(&upper.as_str()) {
        scores.push(("Code", 0.30));
    }

    scores
}

/// Extract alphanumeric tokens from a filename (without extension).
fn extract_tokens(filename: &str) -> Vec<String> {
    let stem = if let Some(dot) = filename.rfind('.') {
        &filename[..dot]
    } else {
        filename
    };
    // Split on non-alphanumeric characters (including underscore)
    stem.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Main heuristic suggestion
// ---------------------------------------------------------------------------

/// Suggest a category for a file based on extension + filename tokens.
/// Returns a (category, confidence) pair where confidence ∈ [0.0, 1.0].
pub fn suggest_category(filename: &str, extension: &str) -> (&'static str, f64) {
    // 1. Extension-based match (strong signal)
    if let Some(cat) = extension_to_category(extension) {
        return (cat, 0.80);
    }

    // 2. No extension or unknown extension → score filename tokens
    let tokens = extract_tokens(filename);
    if tokens.is_empty() {
        return ("Other", 0.0);
    }

    let mut scores: HashMap<&'static str, f64> = HashMap::new();
    for token in &tokens {
        for (cat, boost) in score_filename_token(token) {
            *scores.entry(cat).or_insert(0.0) += boost;
        }
    }

    // Also check if the whole filename (to_uppercase) is a known token
    let whole = filename.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
    for (cat, boost) in score_filename_token(whole) {
        *scores.entry(cat).or_insert(0.0) += boost;
    }

    // Pick the category with the highest score
    let best = scores.into_iter().max_by(|a, b| a.1.total_cmp(&b.1));

    match best {
        Some((cat, score)) if score > 0.0 => {
            // Clamp confidence to [0.3, 0.75] for token-based predictions
            let confidence = score.clamp(0.30, 0.75);
            (cat, confidence)
        }
        _ => ("Other", 0.0),
    }
}

// ---------------------------------------------------------------------------
// Learned hints (co-occurrence from action_logs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnedHint {
    /// The extension or filename token that was observed.
    pub key: String,
    /// The category most often associated with this key.
    pub category: String,
    /// How many times this association was observed.
    pub count: i64,
    /// Relative confidence boost: count / total_observations for this key.
    pub confidence: f64,
}

/// Read the action_logs table and mine extension/name-token → destination
/// category associations. Returns a list of hints sorted by count descending.
pub fn learned_hints() -> Vec<LearnedHint> {
    let logs = match db::get_recent_logs(1000) {
        Ok(logs) => logs,
        Err(_) => return Vec::new(),
    };

    // Map from extension → (category → count)
    let mut ext_map: HashMap<String, HashMap<String, i64>> = HashMap::new();
    // Map from filename token → (category → count)
    let mut token_map: HashMap<String, HashMap<String, i64>> = HashMap::new();

    for log in &logs {
        let ext = log
            .file_name
            .rfind('.')
            .and_then(|dot| log.file_name.get(dot + 1..))
            .unwrap_or("")
            .to_lowercase();
        if ext.is_empty() {
            continue;
        }
        // Use the action type as the "category" proxy (we store the rule name
        // in file_type which is the category name like "Images", "Documents", etc.)
        let category = log.file_type.clone();
        if category.is_empty() {
            continue;
        }

        *ext_map
            .entry(ext.clone())
            .or_default()
            .entry(category.clone())
            .or_insert(0) += 1;

        // Also index filename tokens
        let tokens = extract_tokens(&log.file_name);
        for token in tokens {
            if token.len() < 3 {
                continue;
            }
            *token_map
                .entry(token.to_lowercase())
                .or_default()
                .entry(category.clone())
                .or_insert(0) += 1;
        }
    }

    let mut hints = Vec::new();

    // Process extension-based hints
    for (ext, cat_counts) in &ext_map {
        let total: i64 = cat_counts.values().sum();
        if let Some((best_cat, best_count)) = cat_counts.iter().max_by_key(|(_, c)| **c) {
            if *best_count >= 2 {
                hints.push(LearnedHint {
                    key: format!(".{}", ext),
                    category: best_cat.clone(),
                    count: *best_count,
                    confidence: *best_count as f64 / total as f64,
                });
            }
        }
    }

    // Process token-based hints
    for (token, cat_counts) in &token_map {
        let total: i64 = cat_counts.values().sum();
        if let Some((best_cat, best_count)) = cat_counts.iter().max_by_key(|(_, c)| **c) {
            if *best_count >= 2 {
                hints.push(LearnedHint {
                    key: token.clone(),
                    category: best_cat.clone(),
                    count: *best_count,
                    confidence: *best_count as f64 / total as f64,
                });
            }
        }
    }

    // Sort by count descending, limit to top 50
    hints.sort_by_key(|a| std::cmp::Reverse(a.count));
    hints.truncate(50);
    hints
}

// ---------------------------------------------------------------------------
// AiProvider trait
// ---------------------------------------------------------------------------

pub trait AiProvider: Send {
    /// Classify a filename. Returns `Ok(Some(category))` on success,
    /// `Ok(None)` if the provider cannot decide, or `Err(String)` on failure.
    fn classify(&self, filename: &str) -> Result<Option<String>, String>;
}

// ---------------------------------------------------------------------------
// HeuristicProvider (always available)
// ---------------------------------------------------------------------------

pub struct HeuristicProvider;

impl AiProvider for HeuristicProvider {
    fn classify(&self, filename: &str) -> Result<Option<String>, String> {
        let ext = filename
            .rfind('.')
            .and_then(|dot| filename.get(dot + 1..))
            .unwrap_or("");
        let (category, confidence) = suggest_category(filename, ext);
        if confidence > 0.0 && category != "Other" {
            Ok(Some(category.to_string()))
        } else {
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// OllamaProvider (optional, runtime-detected)
// ---------------------------------------------------------------------------

pub struct OllamaProvider;

impl OllamaProvider {
    /// Check if Ollama is running by hitting the tags endpoint.
    pub fn is_available() -> bool {
        #[cfg(not(test))]
        {
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(2))
                .timeout(std::time::Duration::from_secs(4))
                .build();
            match agent.get("http://localhost:11434/api/tags").call() {
                Ok(resp) => resp.status() == 200,
                Err(_) => false,
            }
        }
        #[cfg(test)]
        {
            false // never available in tests
        }
    }
}

impl AiProvider for OllamaProvider {
    fn classify(&self, filename: &str) -> Result<Option<String>, String> {
        let prompt = format!(
            r#"You are a file classification assistant. Given a filename, classify it into exactly one of these categories: Documents, Images, Videos, Audio, Archives, Code, Other.

Rules:
- Documents: PDFs, text files, office documents, spreadsheets, presentations
- Images: photos, graphics, screenshots, drawings
- Videos: movie files, video clips, recordings
- Audio: music, voice recordings, podcasts
- Archives: compressed files, installers, disk images, backups
- Code: source code, config files, scripts, data files
- Other: anything else

Respond with ONLY a JSON object in this exact format:
{{"category": "CategoryName"}}

Filename: {}"#,
            filename
        );

        let body = serde_json::json!({
            "model": "llama3.2",
            "prompt": prompt,
            "stream": false,
            "format": "json"
        });

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(2))
            .timeout(std::time::Duration::from_secs(15))
            .build();

        let raw_body = serde_json::to_string(&body)
            .map_err(|e| format!("Failed to serialize request body: {}", e))?;
        let response = agent
            .post("http://localhost:11434/api/generate")
            .set("Content-Type", "application/json")
            .send_string(&raw_body)
            .map_err(|e| format!("Ollama request failed: {}", e))?;

        let resp_body = response
            .into_string()
            .map_err(|e| format!("Ollama response read failed: {}", e))?;

        let resp_json: serde_json::Value = serde_json::from_str(&resp_body)
            .map_err(|e| format!("Ollama response parse failed: {}", e))?;

        // The model output is in the "response" field
        let model_output = resp_json
            .get("response")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Ollama response missing 'response' field".to_string())?;

        // Parse the model output as JSON (it should be {"category": "..."})
        let parsed: serde_json::Value =
            serde_json::from_str(model_output).map_err(|e| format!("Failed to parse model output as JSON: {}", e))?;

        let category = parsed
            .get("category")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Model output missing 'category' field".to_string())?;

        let valid = [
            "Documents", "Images", "Videos", "Audio", "Archives", "Code", "Other",
        ];
        if valid.contains(&category) {
            Ok(Some(category.to_string()))
        } else {
            Err(format!("Unknown category from model: {}", category))
        }
    }
}

// ---------------------------------------------------------------------------
// Provider detection
// ---------------------------------------------------------------------------

/// Detect the best available provider. Returns OllamaProvider if Ollama is
/// reachable, otherwise HeuristicProvider.
pub fn detect_provider() -> Box<dyn AiProvider> {
    if OllamaProvider::is_available() {
        Box::new(OllamaProvider)
    } else {
        Box::new(HeuristicProvider)
    }
}

// ---------------------------------------------------------------------------
// Suggestion type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub path: String,
    pub filename: String,
    pub suggested_category: String,
    pub confidence: f64,
    pub source: String, // "heuristic" | "ollama" | "learned"
    pub current_category: String,
}

/// Generate suggestions for files in the inventory that are currently
/// classified as "Other" (or unclassified). Uses heuristic + learned hints,
/// and optionally Ollama if available.
pub fn get_suggestions(limit: usize, provider: &dyn AiProvider) -> Vec<Suggestion> {
    // Get unclassified files from the inventory
    let unclassified = match db::get_unclassified_files((limit * 2) as i64) {
        Ok(files) => files,
        Err(_) => return Vec::new(),
    };

    // Get dismissed suggestions to filter them out
    let dismissed = db::get_dismissed_suggestions().unwrap_or_default();

    // Get learned hints
    let hints = learned_hints();
    // Build a lookup: extension → (category, confidence)
    let mut hint_by_ext: HashMap<String, (&str, f64)> = HashMap::new();
    let mut hint_by_token: HashMap<String, (&str, f64)> = HashMap::new();
    for hint in &hints {
        if hint.key.starts_with('.') {
            hint_by_ext.insert(hint.key.trim_start_matches('.').to_string(), (hint.category.as_str(), hint.confidence));
        } else {
            hint_by_token.insert(hint.key.to_lowercase(), (hint.category.as_str(), hint.confidence));
        }
    }

    let mut suggestions = Vec::new();
    // Bound sequential Ollama round-trips per invocation so a slow model
    // cannot stall the UI for the whole suggestion list.
    let mut ollama_calls_left = 5;

    for file in &unclassified {
        if dismissed.contains(&file.path) {
            continue;
        }

        let filename = std::path::Path::new(&file.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&file.path)
            .to_string();

        let ext = filename
            .rfind('.')
            .and_then(|dot| filename.get(dot + 1..))
            .unwrap_or("")
            .to_lowercase();

        let mut best_category: Option<String> = None;
        let mut best_confidence = 0.0f64;
        let mut best_source = "heuristic".to_string();

        // 1. Heuristic classification
        let (heuristic_cat, heuristic_conf) = suggest_category(&filename, &ext);
        if heuristic_conf > best_confidence {
            best_confidence = heuristic_conf;
            best_category = Some(heuristic_cat.to_string());
            best_source = "heuristic".to_string();
        }

        // 2. Learned hints from action_logs
        if let Some((cat, conf)) = hint_by_ext.get(&ext) {
            if *conf > best_confidence {
                best_confidence = *conf;
                best_category = Some(cat.to_string());
                best_source = "learned".to_string();
            }
        }
        // Check filename tokens for learned hints
        let tokens = extract_tokens(&filename);
        for token in &tokens {
            if let Some((cat, conf)) = hint_by_token.get(&token.to_lowercase()) {
                if *conf > best_confidence {
                    best_confidence = *conf;
                    best_category = Some(cat.to_string());
                    best_source = "learned".to_string();
                }
            }
        }

        // 3. Ollama (if available) — only for files where heuristic is uncertain
        if best_confidence < 0.6 && ollama_calls_left > 0 {
            ollama_calls_left -= 1;
            if let Ok(Some(ai_cat)) = provider.classify(&filename) {
                if ai_cat != "Other" {
                    best_confidence = 0.85;
                    best_category = Some(ai_cat);
                    best_source = "ollama".to_string();
                }
            }
        }

        if let Some(category) = best_category {
            if category != "Other" && category != file.category {
                suggestions.push(Suggestion {
                    path: file.path.clone(),
                    filename,
                    suggested_category: category,
                    confidence: best_confidence,
                    source: best_source,
                    current_category: file.category.clone(),
                });
            }
        }

        if suggestions.len() >= limit {
            break;
        }
    }

    // Sort by confidence descending
    suggestions.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    suggestions
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── suggest_category pure fn tests (≥6 cases) ───────────────────────

    #[test]
    fn test_suggest_by_extension() {
        // Extension-based: strong confidence
        let (cat, conf) = suggest_category("report.pdf", "pdf");
        assert_eq!(cat, "Documents");
        assert!((conf - 0.80).abs() < 1e-9);

        let (cat, conf) = suggest_category("photo.jpg", "jpg");
        assert_eq!(cat, "Images");
        assert!((conf - 0.80).abs() < 1e-9);

        let (cat, conf) = suggest_category("video.mp4", "mp4");
        assert_eq!(cat, "Videos");
        assert!((conf - 0.80).abs() < 1e-9);

        let (cat, conf) = suggest_category("song.mp3", "mp3");
        assert_eq!(cat, "Audio");
        assert!((conf - 0.80).abs() < 1e-9);

        let (cat, conf) = suggest_category("archive.zip", "zip");
        assert_eq!(cat, "Archives");
        assert!((conf - 0.80).abs() < 1e-9);

        let (cat, conf) = suggest_category("main.rs", "rs");
        assert_eq!(cat, "Code");
        assert!((conf - 0.80).abs() < 1e-9);
    }

    #[test]
    fn test_suggest_by_filename_token() {
        // Token-based: "invoice" → Documents
        let (cat, conf) = suggest_category("invoice_2024.pdf", "pdf");
        // Extension wins for known extensions
        assert_eq!(cat, "Documents");
        assert!(conf >= 0.80);

        // No extension → token scoring
        let (cat, conf) = suggest_category("IMG_20240824", "");
        assert_eq!(cat, "Images");
        assert!(conf >= 0.30);

        let (cat, conf) = suggest_category("Screenshot_2024-08-24", "png");
        assert_eq!(cat, "Images");
        assert!(conf >= 0.80);

        let (cat, conf) = suggest_category("setup_installer", "");
        assert_eq!(cat, "Archives");
        assert!(conf >= 0.30);

        let (cat, conf) = suggest_category("budget_2024", "xlsx");
        assert_eq!(cat, "Documents");
        assert!(conf >= 0.80);
    }

    #[test]
    fn test_suggest_unknown_returns_other() {
        let (cat, conf) = suggest_category("junk.xyz", "xyz");
        assert_eq!(cat, "Other");
        assert_eq!(conf, 0.0);

        let (cat, conf) = suggest_category("readme", "");
        assert_eq!(cat, "Other");
        assert_eq!(conf, 0.0);
    }

    #[test]
    fn test_suggest_case_insensitive_extension() {
        let (cat, conf) = suggest_category("Report.PDF", "PDF");
        assert_eq!(cat, "Documents");
        assert!((conf - 0.80).abs() < 1e-9);

        let (cat, _conf) = suggest_category("Photo.JPEG", "JPEG");
        assert_eq!(cat, "Images");
    }

    #[test]
    fn test_suggest_filename_only_no_extension() {
        let (cat, conf) = suggest_category("INVOICE", "");
        assert_eq!(cat, "Documents");
        assert!(conf >= 0.30);

        let (cat, conf) = suggest_category("DSC_1234", "");
        assert_eq!(cat, "Images");
        assert!(conf >= 0.30);
    }

    #[test]
    fn test_suggest_screenshot_variants() {
        let (cat, _conf) = suggest_category("Screenshot 2024-01-01 at 10.30.00.png", "png");
        assert_eq!(cat, "Images");

        let (cat, _conf) = suggest_category("screen-recording-2024.mov", "mov");
        assert_eq!(cat, "Videos");
    }

    // ── extract_tokens tests ──────────────────────────────────────────

    #[test]
    fn test_extract_tokens_basic() {
        let tokens = extract_tokens("invoice_2024.pdf");
        assert!(tokens.contains(&"invoice".to_string()));
        assert!(tokens.contains(&"2024".to_string()));
        assert!(!tokens.contains(&"pdf".to_string()));
    }

    #[test]
    fn test_extract_tokens_no_extension() {
        let tokens = extract_tokens("README");
        assert_eq!(tokens, vec!["README"]);
    }

    #[test]
    fn test_extract_tokens_splits_underscores() {
        let tokens = extract_tokens("setup_installer_v2");
        assert_eq!(tokens, vec!["setup", "installer", "v2"]);
    }

    // ── Ollama JSON parse fallback tests ───────────────────────────────
    // Unit-test the JSON parsing logic that OllamaProvider uses.

    fn parse_ollama_response(body: &str) -> Result<Option<String>, String> {
        let resp_json: serde_json::Value =
            serde_json::from_str(body).map_err(|e| format!("JSON parse failed: {}", e))?;
        let model_output = resp_json
            .get("response")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing response field".to_string())?;
        let parsed: serde_json::Value =
            serde_json::from_str(model_output).map_err(|e| format!("Model output parse failed: {}", e))?;
        let category = parsed
            .get("category")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing category field".to_string())?;
        let valid = [
            "Documents", "Images", "Videos", "Audio", "Archives", "Code", "Other",
        ];
        if valid.contains(&category) {
            Ok(Some(category.to_string()))
        } else {
            Err(format!("Unknown category: {}", category))
        }
    }

    #[test]
    fn test_ollama_parse_valid_json() {
        let body = r#"{"response": "{\"category\": \"Documents\"}"}"#;
        let result = parse_ollama_response(body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("Documents".to_string()));
    }

    #[test]
    fn test_ollama_parse_valid_json_other() {
        let body = r#"{"response": "{\"category\": \"Images\"}"}"#;
        let result = parse_ollama_response(body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("Images".to_string()));
    }

    #[test]
    fn test_ollama_parse_bad_json_fallback() {
        // Invalid JSON in model output → error (caller falls back to heuristic)
        let body = r#"{"response": "not valid json at all"}"#;
        let result = parse_ollama_response(body);
        assert!(result.is_err());
    }

    #[test]
    fn test_ollama_parse_missing_response_field() {
        let body = r#"{"wrong": "data"}"#;
        let result = parse_ollama_response(body);
        assert!(result.is_err());
    }

    #[test]
    fn test_ollama_parse_invalid_category() {
        let body = r#"{"response": "{\"category\": \"InvalidCategory\"}"}"#;
        let result = parse_ollama_response(body);
        assert!(result.is_err());
    }

    // ── HeuristicProvider classify ─────────────────────────────────────

    #[test]
    fn test_heuristic_provider_classify() {
        let provider = HeuristicProvider;
        let result = provider.classify("report.pdf").unwrap();
        assert_eq!(result, Some("Documents".to_string()));

        let result = provider.classify("unknown.xyz").unwrap();
        assert_eq!(result, None);
    }

    // ── learned_hints blending (DB-backed) ─────────────────────────────

    #[test]
    fn test_learned_hints_blending() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();
        let now = chrono::Utc::now();

        // 3 .xyz files moved into Documents, 1 moved into Other
        let docs = [
            ("invoice_a.xyz", "/tmp/Documents/invoice_a.xyz"),
            ("invoice_b.xyz", "/tmp/Documents/invoice_b.xyz"),
            ("invoice_c.xyz", "/tmp/Documents/invoice_c.xyz"),
        ];
        for (name, dest) in docs {
            db::log_action(&db::ActionLog {
                id: None,
                timestamp: now,
                source_path: format!("/tmp/{}", name),
                destination_path: Some(dest.to_string()),
                action: "move".to_string(),
                file_name: name.to_string(),
                file_type: "Documents".to_string(),
                undone: false,
            })
            .unwrap();
        }
        db::log_action(&db::ActionLog {
            id: None,
            timestamp: now,
            source_path: "/tmp/invoice_d.xyz".to_string(),
            destination_path: Some("/tmp/Other/invoice_d.xyz".to_string()),
            action: "move".to_string(),
            file_name: "invoice_d.xyz".to_string(),
            file_type: "Other".to_string(),
            undone: false,
        })
        .unwrap();

        let hints = learned_hints();

        // Extension hint: .xyz → Documents (3 of 4 observations = 0.75)
        let ext_hint = hints.iter().find(|h| h.key == ".xyz").expect(".xyz hint");
        assert_eq!(ext_hint.category, "Documents");
        assert_eq!(ext_hint.count, 3);
        assert!((ext_hint.confidence - 0.75).abs() < 1e-9);

        // Token hint: invoice → Documents (3 of 4 = 0.75)
        let token_hint = hints
            .iter()
            .find(|h| h.key == "invoice")
            .expect("invoice token hint");
        assert_eq!(token_hint.category, "Documents");
        assert_eq!(token_hint.count, 3);
        assert!((token_hint.confidence - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_learned_hints_no_minority_categories() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();
        let now = chrono::Utc::now();

        // A single observation must not produce a hint (count >= 2 required).
        db::log_action(&db::ActionLog {
            id: None,
            timestamp: now,
            source_path: "/tmp/rare.zzz".to_string(),
            destination_path: Some("/tmp/Documents/rare.zzz".to_string()),
            action: "move".to_string(),
            file_name: "rare.zzz".to_string(),
            file_type: "Documents".to_string(),
            undone: false,
        })
        .unwrap();

        let hints = learned_hints();
        assert!(!hints.iter().any(|h| h.key == ".zzz"));
    }

    #[test]
    fn test_learned_hints_sorted_by_count_desc() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();
        let now = chrono::Utc::now();

        // 5 records of .aaa → Images, 2 records of .bbb → Audio
        for i in 0..5 {
            db::log_action(&db::ActionLog {
                id: None,
                timestamp: now,
                source_path: format!("/tmp/img{}.aaa", i),
                destination_path: Some(format!("/tmp/Images/img{}.aaa", i)),
                action: "move".to_string(),
                file_name: format!("img{}.aaa", i),
                file_type: "Images".to_string(),
                undone: false,
            })
            .unwrap();
        }
        for i in 0..2 {
            db::log_action(&db::ActionLog {
                id: None,
                timestamp: now,
                source_path: format!("/tmp/snd{}.bbb", i),
                destination_path: Some(format!("/tmp/Audio/snd{}.bbb", i)),
                action: "move".to_string(),
                file_name: format!("snd{}.bbb", i),
                file_type: "Audio".to_string(),
                undone: false,
            })
            .unwrap();
        }

        let hints = learned_hints();
        let aaa = hints.iter().position(|h| h.key == ".aaa");
        let bbb = hints.iter().position(|h| h.key == ".bbb");
        assert!(aaa.is_some());
        assert!(bbb.is_some());
        assert!(aaa.unwrap() < bbb.unwrap(), ".aaa (count 5) must sort before .bbb (count 2)");
    }

    /// With no unclassified files in the inventory, get_suggestions returns empty.
    #[test]
    fn test_suggestions_empty_without_inventory() {
        let _guard = db::TEST_DB_LOCK.lock().unwrap();
        db::init_test_db();
        let provider = HeuristicProvider;
        let suggestions = get_suggestions(10, &provider);
        assert!(suggestions.is_empty());
    }
}