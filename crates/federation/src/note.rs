//! Clip ↔ Note interop (ARCHITECTURE.md §4, VERIFIED against Loops).
//!
//! Clips are published as `Create(Note)` whose attachment is a `Document`
//! with `mediaType: video/mp4`. Mastodon/Pixelfed render the player INLINE;
//! text-only clients show caption+link. Inbound validation mirrors Loops'
//! rules: the attachment array must contain a `Document`|`Video` whose
//! mediaType is `video/mp4` — anything else is stored unprocessed and never
//! becomes a clip.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use toottok_db::actor::Actor;
use toottok_db::clip::Clip;

/// AP public collection marker (cc on every public clip).
pub const AS_PUBLIC: &str = "https://www.w3.org/ns/activitystreams#Public";

/// Strip ALL html tags, keeping the text, then HTML-escape the remainder.
/// v1 sanitization stance for remote captions (ARCHITECTURE §8 allowlist
/// lands later); also used on local uploads so both directions store plain
/// text.
///
/// F9 hardening: a single strip pass lets hostile payloads smuggle markup —
/// `&lt;img onerror=…&gt;` survives one tag-strip as text but renders as an
/// element once the client entity-decodes it. So: decode entities → strip
/// tags → repeat UNTIL STABLE (bounded loop), then HTML-escape whatever
/// characters remain so nothing stored can ever be interpreted as markup
/// downstream.
pub fn strip_html_tags(input: &str) -> String {
    let mut current = input.to_string();
    // Bounded convergence loop: each pass either removes something or we
    // stop. 8 passes absorb any realistic double-encoding depth; a payload
    // crafted to oscillate past the bound simply keeps its escaped remains.
    for _ in 0..8 {
        let decoded = decode_html_entities(&current);
        let stripped = strip_tags(&decoded);
        if stripped == current {
            break;
        }
        current = stripped;
    }
    escape_html(&current)
}

/// Remove `<...>` sequences, keeping text outside them. Unterminated `<`
/// swallows the rest of that pass (the escape step below guarantees the
/// result is still inert).
fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut depth = 0usize;
    for ch in input.chars() {
        match ch {
            '<' => depth += 1,
            // Only a '>' that closes an open tag is markup; a stray '>' in
            // plain text ("5 > 3") survives and gets escaped later.
            '>' if depth > 0 => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Decode the HTML entities a remote caption realistically carries: the five
/// named XML entities plus decimal/hex numeric references. Unknown or
/// malformed entities are left untouched (they are escaped at the end).
fn decode_html_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < input.len() {
        if bytes[i] != b'&' {
            // Advance by one full char (multi-byte safe).
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&input[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        match decode_one_entity(&input[i..]) {
            Some((decoded, consumed)) => {
                out.push_str(&decoded);
                i += consumed;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

fn utf8_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Try to decode one entity at the start of `rest` (which begins with `&`).
/// Returns `(decoded, bytes_consumed)` on success.
fn decode_one_entity(rest: &str) -> Option<(String, usize)> {
    const NAMED: [(&str, &str); 5] = [
        ("amp", "&"),
        ("lt", "<"),
        ("gt", ">"),
        ("quot", "\""),
        ("apos", "'"),
    ];
    debug_assert!(rest.starts_with('&'));
    let after = &rest[1..];
    for (name, decoded) in NAMED {
        let with_semi = format!("{name};");
        if after.starts_with(&with_semi) {
            return Some((decoded.to_string(), name.len() + 2));
        }
    }
    // Numeric: &#123; / &#x1A;
    if let Some(hex) = after
        .strip_prefix("#x")
        .or_else(|| after.strip_prefix("#X"))
    {
        let end = hex.find(';')?;
        if end > 0 && hex[..end].chars().all(|c| c.is_ascii_hexdigit()) {
            // consumed = '&' + "#x" + digits + ';' = 1 + 2 + end + 1
            return u32::from_str_radix(&hex[..end], 16)
                .ok()
                .and_then(char::from_u32)
                .map(|c| (c.to_string(), end + 4));
        }
        return None;
    }
    if let Some(dec) = after.strip_prefix('#') {
        let end = dec.find(';')?;
        if end > 0 && dec[..end].bytes().all(|b| b.is_ascii_digit()) {
            return dec[..end]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .map(|c| (c.to_string(), end + 3));
        }
    }
    None
}

/// HTML-escape the five markup-significant characters (& first).
pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

/// The canonical object id of a local clip.
pub fn clip_object_id(base_url: &str, clip_id: i64) -> String {
    format!("{base_url}/clips/{clip_id}")
}

/// Fallback public media filename when no rendition row exists yet.
pub const DEFAULT_MEDIA_FILENAME: &str = "720.mp4";

/// Build the `Note` object JSON for a clip per the Loops wire shape. The
/// clip's `ap_id` must already be canonical (`{base}/clips/{id}`), which the
/// finalize path guarantees before this is called.
///
/// `media_filename` selects which stored rendition the attachment URL points
/// at (F5): callers MUST pass the LARGEST AVAILABLE mp4 rendition from
/// `media_assets` (`MediaAsset::largest_video_filename`) so sub-720p sources
/// do not federate URLs that 404. `None` falls back to `720.mp4`.
pub fn clip_note_json(
    base_url: &str,
    clip: &Clip,
    author: &Actor,
    media_filename: Option<&str>,
) -> Value {
    let published = clip.published_at.unwrap_or(clip.created_at).to_rfc3339();
    let mut note = json!({
        "id": clip.ap_id,
        "type": "Note",
        "attributedTo": author.ap_id,
        "content": clip.caption_html.clone().unwrap_or_default(),
        "published": published,
        "url": clip.ap_id,
        "sensitive": clip.is_sensitive,
        "attachment": [attachment_json(base_url, clip, media_filename)],
        "tag": [],
    });
    if let Some(cw) = clip.cw_text.as_deref() {
        note["summary"] = json!(cw);
    }
    note
}

/// One `Document` attachment describing the largest available public mp4
/// rendition.
fn attachment_json(base_url: &str, clip: &Clip, media_filename: Option<&str>) -> Value {
    let filename = media_filename.unwrap_or(DEFAULT_MEDIA_FILENAME);
    let mut attachment = json!({
        "type": "Document",
        "mediaType": "video/mp4",
        "url": format!("{base_url}/assets/{}/{}", clip.id, filename),
    });
    if let Some(w) = clip.width {
        attachment["width"] = json!(w);
    }
    if let Some(h) = clip.height {
        attachment["height"] = json!(h);
    }
    if let Some(d) = clip.duration_s {
        attachment["duration"] = json!((d * 1000.0).round() / 1000.0);
    }
    attachment
}

/// Build the outbound `Create(Note)` activity for a finalized local clip:
/// addressed to the author's followers collection, cc public. See
/// [`clip_note_json`] for the `media_filename` rendition contract (F5).
pub fn clip_create_activity(
    base_url: &str,
    clip: &Clip,
    author: &Actor,
    media_filename: Option<&str>,
) -> Value {
    let published = clip.published_at.unwrap_or(clip.created_at).to_rfc3339();
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{}/activity", clip.ap_id),
        "type": "Create",
        "actor": author.ap_id,
        "published": published,
        "to": [author.followers_url],
        "cc": [AS_PUBLIC],
        "object": clip_note_json(base_url, clip, author, media_filename),
    })
}

/// One validated mp4 attachment from an inbound Note.
#[derive(Debug, Clone)]
pub struct ParsedAttachment {
    pub media_url: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_s: Option<f64>,
}

/// A validated inbound `Note` (the embedded object of `Create`/`Update`).
#[derive(Debug, Clone)]
pub struct ParsedNote {
    pub id: String,
    pub attributed_to: String,
    /// Raw `content` HTML (`None` when the field is absent).
    pub content_html: Option<String>,
    pub sensitive: bool,
    pub summary: Option<String>,
    pub published: Option<DateTime<Utc>>,
    pub attachment: ParsedAttachment,
}

/// Validate an inbound Note per the Loops interop rules: `type: Note`, an
/// `attributedTo`, and an attachment array containing a
/// `Document`|`Video` entry with `mediaType: video/mp4`. Any other shape is
/// rejected (stored unprocessed, never turned into a clip).
pub fn parse_inbound_note(object: &Value) -> Result<ParsedNote, String> {
    if object.get("type").and_then(Value::as_str) != Some("Note") {
        return Err("object is not a Note".into());
    }
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "note has no id".to_string())?
        .to_string();

    let attributed_to = first_string(object.get("attributedTo"))
        .ok_or_else(|| "note has no attributedTo".to_string())?;

    let attachment = object
        .get("attachment")
        .and_then(find_mp4_attachment)
        .ok_or_else(|| {
            "attachment array has no Document/Video with mediaType video/mp4".to_string()
        })?;

    let published = object
        .get("published")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));

    Ok(ParsedNote {
        id,
        attributed_to,
        content_html: object
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string),
        sensitive: object
            .get("sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary: object
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_string),
        published,
        attachment,
    })
}

/// Lenient field extraction for `Update(Note)`: caption/sensitivity/CW only,
/// no attachment requirement.
pub struct NoteFields {
    pub content_html: Option<String>,
    pub sensitive: bool,
    pub summary: Option<String>,
}

pub fn extract_note_fields(object: &Value) -> NoteFields {
    NoteFields {
        content_html: object
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string),
        sensitive: object
            .get("sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary: object
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// `attributedTo` may be a bare string or an array (Mastodon-style
/// `[actor, …]`): take the first string inside.
fn first_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.iter().find_map(|v| v.as_str().map(str::to_string)),
        _ => None,
    }
}

/// Find the first attachment entry that satisfies the Loops media rules:
/// `type` `Document` or `Video`, `mediaType` `video/mp4`, and a URL.
/// Accepts a single object or an array of them.
fn find_mp4_attachment(value: &Value) -> Option<ParsedAttachment> {
    let entries: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![value],
        _ => return None,
    };
    for entry in entries {
        let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
        if kind != "Document" && kind != "Video" {
            continue;
        }
        if entry.get("mediaType").and_then(Value::as_str) != Some("video/mp4") {
            continue;
        }
        let Some(url) = entry
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        return Some(ParsedAttachment {
            media_url: url.to_string(),
            width: int_field(entry, "width"),
            height: int_field(entry, "height"),
            duration_s: entry.get("duration").and_then(parse_duration_seconds),
        });
    }
    None
}

fn int_field(entry: &Value, key: &str) -> Option<i32> {
    entry
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|v| i32::try_from(v).ok())
}

/// Duration arrives either as seconds (number / plain numeric string) or an
/// ISO-8601 duration string ("PT15S", "PT1M30S").
fn parse_duration_seconds(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|d| *d >= 0.0),
        Value::String(s) => parse_duration_str(s),
        _ => None,
    }
}

fn parse_duration_str(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if let Ok(secs) = trimmed.parse::<f64>() {
        return (secs >= 0.0).then_some(secs);
    }
    // ISO-8601: P[nD][T[nH][nM][nS]]
    let rest = trimmed.strip_prefix('P')?;
    let (days, time) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    let mut total = parse_component(days, 'D').unwrap_or(0.0) * 86_400.0;
    if let Some(t) = time {
        total += parse_component(t, 'H').unwrap_or(0.0) * 3_600.0;
        total += parse_component(t, 'M').unwrap_or(0.0) * 60.0;
        total += parse_component(t, 'S').unwrap_or(0.0);
    }
    (total >= 0.0).then_some(total)
}

/// Sum every `[number][unit]` pair in `chunk` matching `unit` (case-insensitive).
fn parse_component(chunk: &str, unit: char) -> Option<f64> {
    let lower = chunk.to_ascii_lowercase();
    let unit_lower = unit.to_ascii_lowercase();
    let mut total = 0.0;
    let mut found = false;
    let mut num_start = 0usize;
    for (idx, ch) in lower.char_indices() {
        if ch == unit_lower {
            let parsed: f64 = lower[num_start..idx].parse().ok()?;
            total += parsed;
            found = true;
            num_start = idx + 1;
        } else if !(ch.is_ascii_digit() || ch == '.' || ch == ',') {
            num_start = idx + 1;
        }
    }
    found.then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_all_tags_keeping_text() {
        assert_eq!(strip_html_tags("<p>hello <b>world</b></p>"), "hello world");
        assert_eq!(strip_html_tags("plain"), "plain");
        assert_eq!(strip_html_tags("<br><br/>"), "");
        assert_eq!(strip_html_tags("  spaced  "), "spaced");
    }

    #[test]
    fn entity_smuggled_tags_are_decoded_then_stripped() {
        // F9: a single tag-strip pass lets entity-encoded markup through.
        assert_eq!(
            strip_html_tags("&lt;img src=x onerror=alert(1)&gt;"),
            "",
            "entity-encoded tag must not survive as markup"
        );
        assert_eq!(
            strip_html_tags("&amp;lt;script&amp;gt;evil&amp;lt;/script&amp;gt;"),
            "evil",
            "double-encoded tags unwind to inert inner text (kept, escaped)"
        );
        assert_eq!(
            strip_html_tags("&#60;svg onload=alert(1)&#62;"),
            "",
            "numeric-entity encoded markup is stripped too"
        );
    }

    #[test]
    fn surviving_markup_is_escaped_before_store() {
        // Text that legitimately contains angle brackets/ampersands ends up
        // stored HTML-escaped so no downstream renderer can revive it.
        assert_eq!(strip_html_tags("fish & chips"), "fish &amp; chips");
        assert_eq!(
            strip_html_tags("a < b and c > d"),
            "a  d",
            "unterminated < swallows to the next > (documented stance)"
        );
        assert_eq!(
            strip_html_tags("5 > 3"),
            "5 &gt; 3",
            "stray > survives as escaped text"
        );
        assert_eq!(escape_html("x&<>'\""), "x&amp;&lt;&gt;&#x27;&quot;");
    }

    #[test]
    fn parses_iso_and_plain_durations() {
        assert_eq!(parse_duration_seconds(&json!("PT1M30S")), Some(90.0));
        assert_eq!(parse_duration_seconds(&json!("PT15S")), Some(15.0));
        assert_eq!(parse_duration_seconds(&json!(42.5)), Some(42.5));
        assert_eq!(parse_duration_seconds(&json!("12")), Some(12.0));
        assert_eq!(parse_duration_seconds(&json!("nope")), None);
    }

    const GOOD_NOTE: &str = r#"{
        "id": "https://b.test/clips/9",
        "type": "Note",
        "attributedTo": "https://b.test/users/bob",
        "content": "<p>nice</p>",
        "sensitive": true,
        "summary": "cw here",
        "published": "2026-08-01T10:00:00Z",
        "attachment": [
            { "type": "Link", "href": "https://x/y.mp4", "mediaType": "video/mp4" },
            { "type": "Document", "mediaType": "video/mp4", "url": "https://b.test/assets/9/720.mp4",
              "width": 720, "height": 1280, "duration": "PT3S" }
        ]
    }"#;

    #[test]
    fn accepts_loops_shape() {
        let note = parse_inbound_note(&serde_json::from_str(GOOD_NOTE).unwrap()).expect("valid");
        assert_eq!(note.id, "https://b.test/clips/9");
        assert_eq!(note.attributed_to, "https://b.test/users/bob");
        assert_eq!(note.attachment.media_url, "https://b.test/assets/9/720.mp4");
        assert_eq!(note.attachment.width, Some(720));
        assert_eq!(note.attachment.height, Some(1280));
        assert_eq!(note.attachment.duration_s, Some(3.0));
        assert!(note.sensitive);
        assert_eq!(note.summary.as_deref(), Some("cw here"));
        assert!(note.published.is_some());
    }

    #[test]
    fn rejects_non_mp4_and_missing_attachment() {
        let bad = serde_json::json!({
            "id": "https://b.test/clips/9",
            "type": "Note",
            "attributedTo": "https://b.test/users/bob",
            "attachment": [{ "type": "Document", "mediaType": "image/png", "url": "https://x/a.png" }],
        });
        assert!(parse_inbound_note(&bad).is_err());

        let none = serde_json::json!({
            "id": "https://b.test/clips/9",
            "type": "Note",
            "attributedTo": "https://b.test/users/bob",
        });
        assert!(parse_inbound_note(&none).is_err());
    }

    #[test]
    fn rejects_non_notes_and_unattributed() {
        let article = serde_json::json!({
            "id": "https://b.test/article/1",
            "type": "Article",
            "attributedTo": "https://b.test/users/bob",
            "attachment": [{ "type": "Document", "mediaType": "video/mp4", "url": "https://x/v.mp4" }],
        });
        assert!(parse_inbound_note(&article).is_err());

        let no_attr = serde_json::json!({
            "id": "https://b.test/clips/9",
            "type": "Note",
            "attachment": [{ "type": "Video", "mediaType": "video/mp4", "url": "https://x/v.mp4" }],
        });
        assert!(parse_inbound_note(&no_attr).is_err());
    }

    #[test]
    fn attributed_to_array_takes_first_string() {
        let note = serde_json::json!({
            "id": "https://b.test/clips/9",
            "type": "Note",
            "attributedTo": ["https://b.test/users/bob"],
            "attachment": { "type": "Video", "mediaType": "video/mp4", "url": "https://x/v.mp4" },
        });
        let parsed = parse_inbound_note(&note).expect("valid");
        assert_eq!(parsed.attributed_to, "https://b.test/users/bob");
        assert_eq!(parsed.attachment.media_url, "https://x/v.mp4");
    }

    #[test]
    fn attachment_url_uses_largest_available_rendition() {
        use chrono::TimeZone;
        // F5: the caller resolves the largest stored rendition from
        // media_assets; sub-720p sources must federate their real filename.
        let stamp = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let clip = Clip {
            id: 42,
            actor_id: 1,
            ap_id: "https://a.test/clips/42".into(),
            origin: "local".into(),
            caption_html: None,
            visibility: "public".into(),
            status: "ready".into(),
            duration_s: None,
            sha256_hash: None,
            width: None,
            height: None,
            size_bytes: None,
            remote_media_url: None,
            remote_poster_url: None,
            is_sensitive: false,
            cw_text: None,
            comments_disabled: false,
            downloads_allowed: true,
            like_count: 0,
            comment_count: 0,
            share_count: 0,
            view_count: 0,
            published_at: Some(stamp),
            deleted_at: None,
            created_at: stamp,
            updated_at: stamp,
        };
        let author = Actor {
            id: 1,
            username: "alice".into(),
            domain: None,
            actor_type: "person".into(),
            public_key_pem: String::new(),
            private_key_pem: None,
            inbox_url: "https://a.test/users/alice/inbox".into(),
            shared_inbox_url: None,
            outbox_url: "https://a.test/users/alice/outbox".into(),
            followers_url: "https://a.test/users/alice/followers".into(),
            display_name: None,
            summary: None,
            avatar_path: None,
            header_path: None,
            manually_approves_followers: false,
            is_locked: false,
            suspended_at: None,
            deleted_at: None,
            ap_id: "https://a.test/users/alice".into(),
            created_at: stamp,
            updated_at: stamp,
        };
        let activity = clip_create_activity("https://a.test", &clip, &author, Some("480.mp4"));
        assert_eq!(
            activity["object"]["attachment"][0]["url"].as_str().unwrap(),
            "https://a.test/assets/42/480.mp4",
            "480p source federates its 480.mp4 rung, not a 404ing 720.mp4"
        );

        let note = clip_note_json("https://a.test", &clip, &author, None);
        assert_eq!(
            note["attachment"][0]["url"].as_str().unwrap(),
            "https://a.test/assets/42/720.mp4",
            "absent rendition row falls back to the historical default"
        );
    }
}
