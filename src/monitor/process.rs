use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::clipboard::{
    ClipboardBackend, get_clipboard_image, get_clipboard_text, get_clipboard_types,
};
use crate::history::ClipboardHistory;
use crate::utils::{PID_FILE, POLL_INTERVAL_MS, UI_LOCK_FILE};

// ============================================================================
// PID FILE MANAGEMENT
// ============================================================================

pub fn write_pid_file(data_dir: &PathBuf) -> Result<(), std::io::Error> {
    let pid_path = data_dir.join(PID_FILE);
    fs::write(pid_path, std::process::id().to_string())
}

pub fn remove_pid_file(data_dir: &PathBuf) {
    let _ = fs::remove_file(data_dir.join(PID_FILE));
}

pub fn get_trigger_script_path(data_dir: &PathBuf) -> PathBuf {
    data_dir.join("trigger.sh")
}

pub fn create_trigger_script(data_dir: &PathBuf, binary_path: &str) -> Result<(), std::io::Error> {
    let script_path = get_trigger_script_path(data_dir);

    let script_content = format!(
        r#"#!/bin/bash
BINARY="{}"

# Single-instance guard: if a clipboard UI is already open, just refocus it
# (Hyprland) and bail out instead of spawning another terminal window.
DATA_DIR="${{XDG_DATA_HOME:-$HOME/.local/share}}/clipboard-manager"
LOCK_FILE="$DATA_DIR/{}"
if [ -f "$LOCK_FILE" ] && kill -0 "$(cat "$LOCK_FILE" 2>/dev/null)" 2>/dev/null; then
    if [ -n "$HYPRLAND_INSTANCE_SIGNATURE" ] && command -v hyprctl &> /dev/null; then
        hyprctl dispatch focuswindow "class:floating-clipboard" > /dev/null 2>&1
    fi
    exit 0
fi

if command -v kitty &> /dev/null; then
    kitty --class floating-clipboard \
          --title "Clipboard Manager" \
          -o initial_window_width=900 \
          -o initial_window_height=600 \
          -o remember_window_size=no \
          "$BINARY" --ui &
elif command -v alacritty &> /dev/null; then
    alacritty --class floating-clipboard \
              --title "Clipboard Manager" \
              -o window.dimensions.columns=100 \
              -o window.dimensions.lines=30 \
              -e "$BINARY" --ui &
elif command -v foot &> /dev/null; then
    foot --app-id=floating-clipboard \
         --title="Clipboard Manager" \
         --window-size-chars=100x30 \
         "$BINARY" --ui &
else
    notify-send "Clipboard Manager" "No suitable terminal found"
fi
"#,
        binary_path, UI_LOCK_FILE
    );

    fs::write(&script_path, script_content)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms)?;
    }

    Ok(())
}

// ============================================================================
// POLLING MONITOR (FALLBACK)
// ============================================================================

pub fn monitor_loop(history: Arc<ClipboardHistory>, backend: ClipboardBackend) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    println!("📋 Clipboard monitor started (Polling Fallback)");

    // Hash of what the clipboard showed on the previous poll. Used purely to
    // detect *changes* cheaply without touching disk while the clipboard sits
    // idle; whether a change actually needs recording is decided by
    // `history.is_latest_*`.
    let mut last_seen_text: Option<u64> = None;
    let mut last_seen_image: Option<u64> = None;

    loop {
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));

        // Check for images first (higher priority)
        let types = get_clipboard_types(backend);
        let has_image = types.iter().any(|t| t.starts_with("image/"));

        if has_image {
            if let Some(image_data) = get_clipboard_image(backend) {
                let mut hasher = DefaultHasher::new();
                image_data.hash(&mut hasher);
                let hash = hasher.finish();

                if Some(hash) != last_seen_image {
                    last_seen_image = Some(hash);
                    last_seen_text = None;
                    // Skip only if it's already the newest entry; a manually
                    // deleted image will not be, so it gets re-added.
                    if !history.is_latest_image(&image_data) {
                        if let Err(e) = history.add_image(image_data) {
                            eprintln!("Failed to add image: {}", e);
                        }
                    }
                }
            }
        } else if let Some(content) = get_clipboard_text(backend) {
            let mut hasher = DefaultHasher::new();
            content.hash(&mut hasher);
            let hash = hasher.finish();

            if Some(hash) != last_seen_text {
                last_seen_text = Some(hash);
                last_seen_image = None;
                if !history.is_latest_text(&content) {
                    history.add_text(content);
                }
            }
        }
    }
}
