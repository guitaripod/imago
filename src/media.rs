use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub key: String,
    pub shortcode: String,
    pub index: u32,
    pub url: String,
    pub is_video: bool,
    pub ext: String,
    pub taken_at: Option<i64>,
    pub caption: Option<String>,
    pub media_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub user_id: String,
    pub username: String,
    pub media_count: Option<u64>,
    pub assets: Vec<Asset>,
    /// One entry per post, used for early-stop heuristics.
    pub post_keys: Vec<String>,
    pub has_next: bool,
    pub end_cursor: Option<String>,
}

pub fn stringify_id(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn best_image_url(node: &Value) -> Option<String> {
    if let Some(cands) = node
        .pointer("/image_versions2/candidates")
        .and_then(|v| v.as_array())
    {
        let mut best: Option<(i64, String)> = None;
        for c in cands {
            let url = c.get("url")?.as_str()?.to_string();
            let w = c.get("width").and_then(|x| x.as_f64()).unwrap_or(0.0) as i64;
            let h = c.get("height").and_then(|x| x.as_f64()).unwrap_or(0.0) as i64;
            let px = w * h;
            if best.as_ref().map(|(p, _)| px >= *p).unwrap_or(true) {
                best = Some((px, url));
            }
        }
        if let Some((_, url)) = best {
            return Some(url);
        }
    }
    node.get("display_url")
        .and_then(|v| v.as_str())
        .or_else(|| node.get("display_uri").and_then(|v| v.as_str()))
        .or_else(|| node.get("display_src").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

fn best_video_url(node: &Value) -> Option<String> {
    if let Some(versions) = node.get("video_versions").and_then(|v| v.as_array()) {
        // Prefer last / highest bandwidth entry
        for v in versions.iter().rev() {
            if let Some(url) = v.get("url").and_then(|u| u.as_str()) {
                return Some(url.to_string());
            }
        }
    }
    node.get("video_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn caption_of(node: &Value) -> Option<String> {
    if let Some(text) = node.pointer("/caption/text").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    if let Some(edges) = node
        .pointer("/edge_media_to_caption/edges")
        .and_then(|v| v.as_array())
    {
        if let Some(text) = edges
            .first()
            .and_then(|e| e.pointer("/node/text"))
            .and_then(|v| v.as_str())
        {
            return Some(text.to_string());
        }
    }
    None
}

fn taken_at_of(node: &Value) -> Option<i64> {
    node.get("taken_at")
        .and_then(|v| v.as_f64())
        .map(|f| f as i64)
        .or_else(|| {
            node.get("taken_at_timestamp")
                .and_then(|v| v.as_f64())
                .map(|f| f as i64)
        })
        .or_else(|| node.get("date").and_then(|v| v.as_f64()).map(|f| f as i64))
}

fn is_video_node(node: &Value) -> bool {
    if node.get("is_video").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    matches!(
        node.get("media_type").and_then(|v| v.as_f64()).map(|f| f as i64),
        Some(2)
    )
}

fn shortcode_of(node: &Value) -> Option<String> {
    node.get("code")
        .and_then(|v| v.as_str())
        .or_else(|| node.get("shortcode").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

fn asset_from_node(node: &Value, shortcode: &str, index: u32, multi: bool) -> Option<Asset> {
    let video = is_video_node(node);
    let url = if video {
        best_video_url(node).or_else(|| best_image_url(node))?
    } else {
        best_image_url(node)?
    };
    let ext = if video && url.contains(".mp4") {
        "mp4"
    } else if video {
        "mp4"
    } else {
        "jpg"
    };
    let key = if multi {
        format!("{shortcode}_{index:02}")
    } else {
        shortcode.to_string()
    };
    Some(Asset {
        key,
        shortcode: shortcode.to_string(),
        index,
        url,
        is_video: video,
        ext: ext.to_string(),
        taken_at: taken_at_of(node),
        caption: caption_of(node),
        media_id: stringify_id(node.get("id").unwrap_or(&Value::Null))
            .or_else(|| stringify_id(node.get("pk").unwrap_or(&Value::Null))),
    })
}

/// Expand a single post node (GraphQL or private API shape) into downloadable assets.
pub fn expand_post(node: &Value) -> Vec<Asset> {
    let Some(shortcode) = shortcode_of(node) else {
        return Vec::new();
    };

    // Carousel (private API)
    if let Some(children) = node.get("carousel_media").and_then(|v| v.as_array()) {
        if !children.is_empty() {
            return children
                .iter()
                .enumerate()
                .filter_map(|(i, child)| asset_from_node(child, &shortcode, i as u32, true))
                .collect();
        }
    }

    // Sidecar (classic GraphQL)
    if let Some(edges) = node
        .pointer("/edge_sidecar_to_children/edges")
        .and_then(|v| v.as_array())
    {
        if !edges.is_empty() {
            return edges
                .iter()
                .enumerate()
                .filter_map(|(i, edge)| {
                    let child = edge.get("node")?;
                    asset_from_node(child, &shortcode, i as u32, true)
                })
                .collect();
        }
    }

    asset_from_node(node, &shortcode, 0, false)
        .into_iter()
        .collect()
}

/// Expand a GraphQL edge list (`{ edges: [ { node: ... } ] }`) or feed `items` array.
pub fn expand_connection(data: &Value) -> (Vec<Asset>, Vec<String>, bool, Option<String>) {
    let mut assets = Vec::new();
    let mut post_keys = Vec::new();

    let edges = data
        .get("edges")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if !edges.is_empty() {
        for edge in &edges {
            let node = edge.get("node").unwrap_or(edge);
            let expanded = expand_post(node);
            if let Some(sc) = shortcode_of(node) {
                post_keys.push(sc);
            }
            assets.extend(expanded);
        }
        let has_next = data
            .pointer("/page_info/has_next_page")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cursor = data
            .pointer("/page_info/end_cursor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        return (assets, post_keys, has_next, cursor);
    }

    // Feed items style
    if let Some(items) = data.get("items").and_then(|v| v.as_array()) {
        for item in items {
            let expanded = expand_post(item);
            if let Some(sc) = shortcode_of(item) {
                post_keys.push(sc);
            }
            assets.extend(expanded);
        }
        let has_next = data
            .get("more_available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cursor = data
            .get("next_max_id")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            });
        return (assets, post_keys, has_next, cursor);
    }

    (assets, post_keys, false, None)
}

pub fn is_feed_cursor(cursor: &str) -> bool {
    // Empty = "start/resume feed stream" (used after profile seed to switch
    // off GraphQL end_cursor, which Instagram soft-blocks more aggressively).
    if cursor.is_empty() {
        return true;
    }
    cursor.contains('_')
        && cursor
            .chars()
            .all(|c| c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expand_image() {
        let node = json!({
            "code": "ABC",
            "media_type": 1,
            "image_versions2": {
                "candidates": [
                    {"url": "https://cdn/small.jpg", "width": 100, "height": 100},
                    {"url": "https://cdn/big.jpg", "width": 1080, "height": 1080}
                ]
            }
        });
        let assets = expand_post(&node);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].key, "ABC");
        assert_eq!(assets[0].url, "https://cdn/big.jpg");
        assert!(!assets[0].is_video);
    }

    #[test]
    fn expand_video() {
        let node = json!({
            "shortcode": "VID1",
            "is_video": true,
            "video_versions": [
                {"url": "https://cdn/a.mp4"},
                {"url": "https://cdn/best.mp4"}
            ]
        });
        let assets = expand_post(&node);
        assert_eq!(assets.len(), 1);
        assert!(assets[0].is_video);
        assert_eq!(assets[0].ext, "mp4");
        assert_eq!(assets[0].url, "https://cdn/best.mp4");
    }

    #[test]
    fn expand_carousel() {
        let node = json!({
            "code": "CAR",
            "media_type": 8,
            "carousel_media": [
                {
                    "media_type": 1,
                    "image_versions2": {"candidates": [{"url": "https://cdn/0.jpg", "width": 10, "height": 10}]}
                },
                {
                    "media_type": 2,
                    "video_versions": [{"url": "https://cdn/1.mp4"}]
                }
            ]
        });
        let assets = expand_post(&node);
        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].key, "CAR_00");
        assert_eq!(assets[1].key, "CAR_01");
        assert!(assets[1].is_video);
    }
}
