// Audio device management module
// Re-exports all device-related functionality to preserve API surface

pub mod configuration;
pub mod discovery;
pub mod microphone;
pub mod platform;
pub mod speakers;

// Re-export all public functions to preserve existing API
pub use configuration::{
    device_from_id, get_device_and_config, parse_audio_device, AudioDevice,
    AudioTranscriptionEngine, DeviceControl, DeviceType, LAST_AUDIO_CAPTURE,
};
pub use discovery::{list_audio_devices, trigger_audio_permission, verify_microphone_access};
pub use microphone::{default_input_device, find_builtin_input_device};
pub use speakers::{default_output_device, find_builtin_output_device};

/// Display label for a device ID, for user-facing messages. Falls back to the
/// ID where no label is known.
pub fn device_label(id: &str) -> String {
    #[cfg(target_os = "linux")]
    {
        platform::linux::cached_label(id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        id.to_string()
    }
}
