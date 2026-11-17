// Audio capture implementations module

pub mod backend_config;
pub mod microphone;
pub mod system;

#[cfg(target_os = "macos")]
pub mod core_audio;

#[cfg(target_os = "linux")]
pub mod pulse_linux;

#[cfg(target_os = "linux")]
pub(crate) mod pulse_enumerator;

// Re-export capture functionality
pub use system::{
    check_system_audio_permissions, list_system_audio_devices, start_system_audio_capture,
    SystemAudioCapture, SystemAudioStream,
};

#[cfg(target_os = "macos")]
pub use core_audio::{CoreAudioCapture, CoreAudioStream};

#[cfg(target_os = "linux")]
pub use pulse_linux::{
    default_sink as default_pulse_sink, default_sink_name as default_pulse_sink_name,
    default_source_name as default_pulse_source_name, find_sink_by_name, find_source_by_name,
    list_sinks as list_pulse_sinks, list_sources as list_pulse_sources, PulseCapture, PulseSink,
    PulseSource,
};

// Re-export backend configuration
pub use backend_config::{
    get_available_backends, get_current_backend, set_current_backend, AudioCaptureBackend,
    BackendConfig, BACKEND_CONFIG,
};
