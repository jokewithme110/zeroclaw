//! Helper for trimming old multimodal image markers from conversation history.
//!
//! This file keeps the marker-level trimming logic separate from the broader
//! multimodal normalization pipeline so feature-specific behavior stays
//! localized and `multimodal.rs` remains a thin orchestrator.

use crate::multimodal::{
    latest_tool_result_indices, parse_image_markers, replay_message_without_stale_tool_images,
    should_normalize_message_images,
};
use std::collections::HashSet;
use zeroclaw_api::model_provider::ChatMessage;

/// Strip the oldest image markers until total image count is within
/// `max_images`. Drops individual markers rather than whole messages so the
/// newest surviving images remain available even when one message has
/// accumulated multiple markers across turns.
pub(crate) fn trim_old_images(messages: &[ChatMessage], max_images: usize) -> Vec<ChatMessage> {
    let latest_tool_indices = latest_tool_result_indices(messages);
    let total_images = messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            should_normalize_message_images(*index, message, &latest_tool_indices)
        })
        .map(|(_, message)| parse_image_markers(&message.content).1.len())
        .sum::<usize>();

    let drop_count = total_images.saturating_sub(max_images);
    if drop_count == 0 {
        return messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                replay_message_without_stale_tool_images(index, message, &latest_tool_indices)
            })
            .collect();
    }

    let mut remaining_to_drop = drop_count;
    let mut drop_set = HashSet::new();
    for (msg_idx, message) in messages.iter().enumerate() {
        if remaining_to_drop == 0 {
            break;
        }
        if !should_normalize_message_images(msg_idx, message, &latest_tool_indices) {
            continue;
        }
        let marker_count = parse_image_markers(&message.content).1.len();
        for occurrence_idx in 0..marker_count {
            if remaining_to_drop == 0 {
                break;
            }
            drop_set.insert((msg_idx, occurrence_idx));
            remaining_to_drop -= 1;
        }
    }

    messages
        .iter()
        .enumerate()
        .map(|(msg_idx, message)| {
            if !should_normalize_message_images(msg_idx, message, &latest_tool_indices) {
                return replay_message_without_stale_tool_images(
                    msg_idx,
                    message,
                    &latest_tool_indices,
                );
            }

            let (cleaned, refs) = parse_image_markers(&message.content);
            if refs.is_empty() || !drop_set.iter().any(|&(mi, _)| mi == msg_idx) {
                return replay_message_without_stale_tool_images(
                    msg_idx,
                    message,
                    &latest_tool_indices,
                );
            }

            let kept_refs: Vec<&str> = refs
                .iter()
                .enumerate()
                .filter(|(occurrence_idx, _)| !drop_set.contains(&(msg_idx, *occurrence_idx)))
                .map(|(_, image_ref)| image_ref.as_str())
                .collect();

            if kept_refs.is_empty() {
                let text = if cleaned.trim().is_empty() {
                    "[image removed from history]".to_string()
                } else {
                    cleaned
                };
                return ChatMessage {
                    role: message.role.clone(),
                    content: text,
                };
            }

            let mut content = cleaned.trim().to_string();
            for image_ref in kept_refs {
                if !content.is_empty() {
                    content.push_str("\n\n");
                }
                content.push_str("[IMAGE:");
                content.push_str(image_ref);
                content.push(']');
            }

            ChatMessage {
                role: message.role.clone(),
                content,
            }
        })
        .collect()
}
