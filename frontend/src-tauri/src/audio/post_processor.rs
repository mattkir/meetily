use anyhow::Result;
use log::{error, info, warn};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Post-processing request for transcript text
#[derive(Debug, Clone)]
pub struct PostProcessRequest {
    pub sequence_id: u32,
    pub raw_text: String,
    pub is_partial: bool,
    pub timestamp: String,
}

/// Post-processing response with refined text
#[derive(Debug, Clone)]
pub struct PostProcessResponse {
    pub sequence_id: u32,
    pub processed_text: String,
    pub confidence: f32,
    pub is_partial: bool,
    pub timestamp: String,
    pub processing_time_ms: u64,
}

/// Background post-processing pipeline for transcript text
pub struct PostProcessor {
    request_sender: mpsc::UnboundedSender<PostProcessRequest>,
    response_receiver: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<PostProcessResponse>>>,
    _handle: tokio::task::JoinHandle<()>,
}

impl PostProcessor {
    /// Create a new post-processor with background processing
    pub fn new() -> Self {
        let (request_sender, mut request_receiver) = mpsc::unbounded_channel();
        let (response_sender, response_receiver) = mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            info!("Background post-processor started");

            while let Some(request) = request_receiver.recv().await {
                let start_time = std::time::Instant::now();

                match Self::process_text(&request).await {
                    Ok(processed_text) => {
                        let processing_time = start_time.elapsed().as_millis() as u64;

                        let response = PostProcessResponse {
                            sequence_id: request.sequence_id,
                            processed_text,
                            confidence: if request.is_partial { 0.8 } else { 0.95 }, // Processed text has higher confidence
                            is_partial: request.is_partial,
                            timestamp: request.timestamp,
                            processing_time_ms: processing_time,
                        };

                        if let Err(e) = response_sender.send(response) {
                            error!("Failed to send post-processing response: {}", e);
                            break;
                        }

                        if processing_time > 100 {
                            warn!(
                                "Slow post-processing for sequence {}: {}ms",
                                request.sequence_id, processing_time
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Post-processing failed for sequence {}: {}",
                            request.sequence_id, e
                        );
                        // Send original text as fallback
                        let response = PostProcessResponse {
                            sequence_id: request.sequence_id,
                            processed_text: request.raw_text.clone(),
                            confidence: 0.5, // Lower confidence for failed processing
                            is_partial: request.is_partial,
                            timestamp: request.timestamp,
                            processing_time_ms: start_time.elapsed().as_millis() as u64,
                        };

                        if let Err(e) = response_sender.send(response) {
                            error!("Failed to send fallback response: {}", e);
                            break;
                        }
                    }
                }
            }

            info!("Background post-processor stopped");
        });

        Self {
            request_sender,
            response_receiver: Arc::new(tokio::sync::Mutex::new(response_receiver)),
            _handle: handle,
        }
    }

    /// Submit text for background post-processing
    pub fn process_async(&self, request: PostProcessRequest) -> Result<()> {
        self.request_sender
            .send(request)
            .map_err(|e| anyhow::anyhow!("Failed to submit post-processing request: {}", e))
    }

    /// Try to receive processed results (non-blocking)
    pub async fn try_recv(&self) -> Option<PostProcessResponse> {
        let mut receiver = self.response_receiver.lock().await;
        receiver.try_recv().ok()
    }

    /// Wait for the next processed result
    pub async fn recv(&self) -> Option<PostProcessResponse> {
        let mut receiver = self.response_receiver.lock().await;
        receiver.recv().await
    }

    /// Process text synchronously (for testing or direct use)
    async fn process_text(request: &PostProcessRequest) -> Result<String> {
        Ok(cleanup_transcript(&request.raw_text, !request.is_partial))
    }
}

impl Default for PostProcessor {
    fn default() -> Self {
        Self::new()
    }
}

/// Synchronous heuristic cleanup shared by meeting post-processing and dictation.
/// Fast path: intended for &lt;10ms on short utterances. Never logs text content.
pub fn cleanup_transcript(text: &str, apply_contextual: bool) -> String {
    if text.trim().len() < 3 {
        return text.to_string();
    }

    let deduplicated = clean_repetitive_text(text);
    let cleaned = remove_artifacts(&deduplicated);
    let normalized = normalize_text(&cleaned);

    if apply_contextual {
        apply_contextual_improvements(&normalized)
    } else {
        normalized
    }
}

/// Dictation default polish: full heuristic pass (fillers, punctuation, contractions).
pub fn cleanup_for_dictation(text: &str) -> String {
    cleanup_transcript(text, true)
}

fn clean_repetitive_text(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 4 {
        return text.to_string();
    }

    let mut result = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let current_word = words[i];

        if i + 1 < words.len() && words[i + 1] == current_word {
            result.push(current_word);
            while i + 1 < words.len() && words[i + 1] == current_word {
                i += 1;
            }
        } else if i + 3 < words.len() {
            let phrase = &words[i..i + 2];
            let next_phrase = &words[i + 2..i + 4];

            if phrase == next_phrase {
                result.extend_from_slice(phrase);
                i += 4;

                while i + 1 < words.len() && i + 1 < words.len() - 1 {
                    let check_phrase = &words[i..std::cmp::min(i + 2, words.len())];
                    if check_phrase == phrase && check_phrase.len() == 2 {
                        i += 2;
                    } else {
                        break;
                    }
                }
                continue;
            }
            result.push(current_word);
        } else {
            result.push(current_word);
        }
        i += 1;
    }

    result.join(" ")
}

fn remove_artifacts(text: &str) -> String {
    let mut words: Vec<String> = text.split_whitespace().map(|w| w.to_string()).collect();

    let fillers = [
        "uh", "um", "er", "ah", "oh", "hm", "hmm", "uhh", "umm", "err", "ahh", "ohh",
    ];

    words.retain(|word| {
        let clean_word_temp = word.to_lowercase();
        let clean_word = clean_word_temp.trim_matches(|c: char| !c.is_alphabetic());
        !fillers.contains(&clean_word) || clean_word.len() > 3
    });

    words.join(" ")
}

fn normalize_text(text: &str) -> String {
    let mut normalized = text.trim().to_string();

    normalized = normalized.replace(" .", ".");
    normalized = normalized.replace(" ,", ",");
    normalized = normalized.replace(" ?", "?");
    normalized = normalized.replace(" !", "!");

    normalized = normalized.replace(".  ", ". ");
    normalized = normalized.replace("?  ", "? ");
    normalized = normalized.replace("!  ", "! ");

    if let Some(first_char) = normalized.chars().next() {
        if first_char.is_lowercase() {
            let rest: String = normalized.chars().skip(1).collect();
            normalized = first_char.to_uppercase().collect::<String>() + &rest;
        }
    }

    // Light sentence capitalization after .?!
    let mut out = String::with_capacity(normalized.len());
    let mut capitalize_next = false;
    for ch in normalized.chars() {
        if capitalize_next && ch.is_alphabetic() {
            out.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
            if matches!(ch, '.' | '?' | '!') {
                capitalize_next = true;
            } else if !ch.is_whitespace() {
                capitalize_next = false;
            }
        }
    }
    out
}

fn apply_contextual_improvements(text: &str) -> String {
    let mut improved = text.to_string();

    let corrections = [
        ("cant", "can't"),
        ("wont", "won't"),
        ("dont", "don't"),
        ("doesnt", "doesn't"),
        ("didnt", "didn't"),
        ("wouldnt", "wouldn't"),
        ("couldnt", "couldn't"),
        ("shouldnt", "shouldn't"),
        ("isnt", "isn't"),
        ("arent", "aren't"),
        ("wasnt", "wasn't"),
        ("werent", "weren't"),
        ("hasnt", "hasn't"),
        ("havent", "haven't"),
        ("hadnt", "hadn't"),
    ];

    for (incorrect, correct) in &corrections {
        improved = replace_whole_word_ci(&improved, incorrect, correct);
    }

    improved
}

/// Case-insensitive whole-word replacement (ASCII word chars + apostrophes).
fn replace_whole_word_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() || haystack.is_empty() {
        return haystack.to_string();
    }

    let hay_lower = haystack.to_lowercase();
    let needle_lower = needle.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0usize;

    while let Some(rel) = hay_lower[cursor..].find(&needle_lower) {
        let start = cursor + rel;
        let end = start + needle.len();

        let before_ok = if start == 0 {
            true
        } else {
            haystack[..start]
                .chars()
                .last()
                .map(|c| !is_word_char(c))
                .unwrap_or(true)
        };
        let after_ok = if end >= haystack.len() {
            true
        } else {
            haystack[end..]
                .chars()
                .next()
                .map(|c| !is_word_char(c))
                .unwrap_or(true)
        };

        out.push_str(&haystack[cursor..start]);
        if before_ok && after_ok {
            out.push_str(replacement);
        } else {
            out.push_str(&haystack[start..end]);
        }
        cursor = end;
    }
    out.push_str(&haystack[cursor..]);
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_cleanup_strips_fillers_and_fixes_punctuation() {
        let raw = "um hello world . this is a test";
        let cleaned = cleanup_for_dictation(raw);
        assert!(!cleaned.to_lowercase().contains("um"));
        assert!(cleaned.contains("Hello") || cleaned.starts_with('H'));
        assert!(!cleaned.contains(" ."));
    }

    #[test]
    fn whole_word_replace_is_case_insensitive() {
        assert_eq!(
            replace_whole_word_ci("I cant go", "cant", "can't"),
            "I can't go"
        );
        assert_eq!(
            replace_whole_word_ci("scant chance", "cant", "can't"),
            "scant chance"
        );
    }
}
