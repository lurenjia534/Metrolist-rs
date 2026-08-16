use std::{sync::mpsc, thread, time::Duration};

use gpui::Window;
use notify_rust::{Notification, Timeout};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

use crate::domain::Song;
use crate::services::PlaybackState;

const DEFAULT_SEEK_STEP: Duration = Duration::from_secs(10);
const PROGRESS_PUBLISH_STEP: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq)]
pub enum DesktopMediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    SeekRelative {
        direction: DesktopSeekDirection,
        amount: Duration,
    },
    SetPosition(Duration),
    SetVolume(f32),
    Raise,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSeekDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopPlaybackState {
    Stopped,
    Paused,
    Playing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesktopTrack {
    id: String,
    title: String,
    artist: String,
    cover_url: Option<String>,
    duration: Option<Duration>,
}

impl From<&Song> for DesktopTrack {
    fn from(song: &Song) -> Self {
        Self {
            id: song.video_id.clone(),
            title: song.title.clone(),
            artist: song.artist_line(),
            cover_url: song.thumbnail_url.clone(),
            duration: song.duration,
        }
    }
}

trait DesktopMediaBackend {
    fn drain_commands(&mut self) -> Vec<DesktopMediaCommand>;
    fn set_metadata(&mut self, track: &DesktopTrack) -> std::result::Result<(), String>;
    fn clear_metadata(&mut self) -> std::result::Result<(), String>;
    fn set_playback(
        &mut self,
        state: DesktopPlaybackState,
        position: Duration,
    ) -> std::result::Result<(), String>;
    fn set_volume(&mut self, volume: f32) -> std::result::Result<(), String>;
    fn show_now_playing(&mut self, track: &DesktopTrack) -> std::result::Result<(), String>;
}

/// Keeps native media surfaces synchronized without exposing platform APIs to
/// the UI. Backend failures are deliberately non-fatal: audio remains usable
/// even when a desktop has no notification daemon or media-session service.
pub struct DesktopMediaSession {
    backend: Box<dyn DesktopMediaBackend>,
    last_track_id: Option<String>,
    notified_track_id: Option<String>,
    last_playback_state: Option<DesktopPlaybackState>,
    last_position: Duration,
    last_volume: Option<f32>,
}

impl DesktopMediaSession {
    pub fn new(window: &Window) -> Self {
        Self::with_backend(Box::new(SystemDesktopMediaBackend::new(window)))
    }

    fn with_backend(backend: Box<dyn DesktopMediaBackend>) -> Self {
        Self {
            backend,
            last_track_id: None,
            notified_track_id: None,
            last_playback_state: None,
            last_position: Duration::ZERO,
            last_volume: None,
        }
    }

    pub fn drain_commands(&mut self) -> Vec<DesktopMediaCommand> {
        self.backend.drain_commands()
    }

    pub fn sync(
        &mut self,
        song: Option<&Song>,
        playback_state: PlaybackState,
        position: Duration,
        volume: f32,
    ) {
        let state = desktop_playback_state(playback_state);

        if let Some(song) = song {
            let track = DesktopTrack::from(song);
            if self.last_track_id.as_deref() != Some(track.id.as_str()) {
                log_backend_error(
                    "desktop media metadata update",
                    self.backend.set_metadata(&track),
                );
                self.last_track_id = Some(track.id.clone());
                self.last_position = Duration::ZERO;
            }
            if state == DesktopPlaybackState::Playing
                && self.notified_track_id.as_deref() != Some(track.id.as_str())
            {
                log_backend_error(
                    "now-playing notification",
                    self.backend.show_now_playing(&track),
                );
                self.notified_track_id = Some(track.id);
            }
        } else if self.last_track_id.take().is_some() {
            log_backend_error(
                "desktop media metadata clear",
                self.backend.clear_metadata(),
            );
            self.notified_track_id = None;
        }

        let should_publish_progress = state == DesktopPlaybackState::Playing
            && position.abs_diff(self.last_position) >= PROGRESS_PUBLISH_STEP;
        if self.last_playback_state != Some(state) || should_publish_progress {
            log_backend_error(
                "desktop playback state update",
                self.backend.set_playback(state, position),
            );
            self.last_playback_state = Some(state);
            self.last_position = position;
        }

        let volume = volume.clamp(0.0, 1.0);
        if self
            .last_volume
            .is_none_or(|last_volume| (last_volume - volume).abs() >= 0.001)
        {
            log_backend_error(
                "desktop media volume update",
                self.backend.set_volume(volume),
            );
            self.last_volume = Some(volume);
        }
    }
}

fn desktop_playback_state(state: PlaybackState) -> DesktopPlaybackState {
    match state {
        PlaybackState::Playing => DesktopPlaybackState::Playing,
        PlaybackState::Paused | PlaybackState::Loading => DesktopPlaybackState::Paused,
        PlaybackState::Idle | PlaybackState::Ended | PlaybackState::Failed => {
            DesktopPlaybackState::Stopped
        }
    }
}

fn log_backend_error(operation: &str, result: std::result::Result<(), String>) {
    if let Err(error) = result {
        tracing::warn!(%error, %operation, "desktop integration degraded");
    }
}

struct SystemDesktopMediaBackend {
    controls: Option<MediaControls>,
    commands: mpsc::Receiver<DesktopMediaCommand>,
    notification_commands: mpsc::Sender<NotificationCommand>,
    notification_worker: Option<thread::JoinHandle<()>>,
}

impl SystemDesktopMediaBackend {
    fn new(window: &Window) -> Self {
        let (command_sender, commands) = mpsc::channel();
        let controls = create_media_controls(window, command_sender);
        let (notification_commands, receiver) = mpsc::channel();
        let notification_worker = thread::Builder::new()
            .name("metrolist-notify".into())
            .spawn(move || run_notification_worker(receiver))
            .map_err(|error| {
                tracing::warn!(%error, "desktop notification worker could not start");
            })
            .ok();

        Self {
            controls,
            commands,
            notification_commands,
            notification_worker,
        }
    }

    fn with_controls(
        &mut self,
        update: impl FnOnce(&mut MediaControls) -> std::result::Result<(), souvlaki::Error>,
    ) -> std::result::Result<(), String> {
        let Some(controls) = self.controls.as_mut() else {
            return Ok(());
        };
        update(controls).map_err(|error| error.to_string())
    }
}

impl DesktopMediaBackend for SystemDesktopMediaBackend {
    fn drain_commands(&mut self) -> Vec<DesktopMediaCommand> {
        self.commands.try_iter().collect()
    }

    fn set_metadata(&mut self, track: &DesktopTrack) -> std::result::Result<(), String> {
        self.with_controls(|controls| {
            controls.set_metadata(MediaMetadata {
                title: Some(&track.title),
                artist: Some(&track.artist),
                cover_url: track.cover_url.as_deref(),
                duration: track.duration,
                ..MediaMetadata::default()
            })
        })
    }

    fn clear_metadata(&mut self) -> std::result::Result<(), String> {
        self.with_controls(|controls| controls.set_metadata(MediaMetadata::default()))
    }

    fn set_playback(
        &mut self,
        state: DesktopPlaybackState,
        position: Duration,
    ) -> std::result::Result<(), String> {
        let progress = Some(MediaPosition(position));
        self.with_controls(|controls| {
            controls.set_playback(match state {
                DesktopPlaybackState::Stopped => MediaPlayback::Stopped,
                DesktopPlaybackState::Paused => MediaPlayback::Paused { progress },
                DesktopPlaybackState::Playing => MediaPlayback::Playing { progress },
            })
        })
    }

    fn set_volume(&mut self, volume: f32) -> std::result::Result<(), String> {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.with_controls(|controls| controls.set_volume(f64::from(volume)))
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = volume;
            Ok(())
        }
    }

    fn show_now_playing(&mut self, track: &DesktopTrack) -> std::result::Result<(), String> {
        self.notification_commands
            .send(NotificationCommand::Show {
                title: track.title.clone(),
                artist: track.artist.clone(),
            })
            .map_err(|_| "desktop notification worker stopped unexpectedly".into())
    }
}

impl Drop for SystemDesktopMediaBackend {
    fn drop(&mut self) {
        let _ = self
            .notification_commands
            .send(NotificationCommand::Shutdown);
        if let Some(worker) = self.notification_worker.take() {
            let _ = worker.join();
        }
    }
}

fn create_media_controls(
    window: &Window,
    command_sender: mpsc::Sender<DesktopMediaCommand>,
) -> Option<MediaControls> {
    if !desktop_media_controls_available() {
        tracing::warn!("Linux session D-Bus is unavailable; desktop media controls are disabled");
        return None;
    }

    let config = PlatformConfig {
        dbus_name: "metrolist",
        display_name: "Metrolist",
        hwnd: platform_window_handle(window),
    };
    let mut controls = match MediaControls::new(config) {
        Ok(controls) => controls,
        Err(error) => {
            tracing::warn!(%error, "desktop media controls are unavailable");
            return None;
        }
    };
    if let Err(error) = controls.attach(move |event| {
        if let Some(command) = map_media_control_event(event) {
            let _ = command_sender.send(command);
        }
    }) {
        tracing::warn!(%error, "desktop media controls could not attach");
        return None;
    }
    Some(controls)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn desktop_media_controls_available() -> bool {
    session_bus_probe(zbus::blocking::Connection::session)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn session_bus_probe<T, E>(connect: impl FnOnce() -> std::result::Result<T, E>) -> bool {
    connect().is_ok()
}

#[cfg(not(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
)))]
fn desktop_media_controls_available() -> bool {
    true
}

#[cfg(target_os = "windows")]
fn platform_window_handle(window: &Window) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::RawWindowHandle;

    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.hwnd.get() as *mut std::ffi::c_void)
}

#[cfg(not(target_os = "windows"))]
fn platform_window_handle(_window: &Window) -> Option<*mut std::ffi::c_void> {
    None
}

fn map_media_control_event(event: MediaControlEvent) -> Option<DesktopMediaCommand> {
    match event {
        MediaControlEvent::Play => Some(DesktopMediaCommand::Play),
        MediaControlEvent::Pause => Some(DesktopMediaCommand::Pause),
        MediaControlEvent::Toggle => Some(DesktopMediaCommand::Toggle),
        MediaControlEvent::Next => Some(DesktopMediaCommand::Next),
        MediaControlEvent::Previous => Some(DesktopMediaCommand::Previous),
        MediaControlEvent::Stop => Some(DesktopMediaCommand::Stop),
        MediaControlEvent::Seek(direction) => Some(DesktopMediaCommand::SeekRelative {
            direction: map_seek_direction(direction),
            amount: DEFAULT_SEEK_STEP,
        }),
        MediaControlEvent::SeekBy(direction, amount) => Some(DesktopMediaCommand::SeekRelative {
            direction: map_seek_direction(direction),
            amount,
        }),
        MediaControlEvent::SetPosition(MediaPosition(position)) => {
            Some(DesktopMediaCommand::SetPosition(position))
        }
        MediaControlEvent::SetVolume(volume) => {
            Some(DesktopMediaCommand::SetVolume(volume.clamp(0.0, 1.0) as f32))
        }
        MediaControlEvent::Raise => Some(DesktopMediaCommand::Raise),
        MediaControlEvent::Quit => Some(DesktopMediaCommand::Quit),
        MediaControlEvent::OpenUri(_) => None,
    }
}

fn map_seek_direction(direction: SeekDirection) -> DesktopSeekDirection {
    match direction {
        SeekDirection::Forward => DesktopSeekDirection::Forward,
        SeekDirection::Backward => DesktopSeekDirection::Backward,
    }
}

enum NotificationCommand {
    Show { title: String, artist: String },
    Shutdown,
}

fn run_notification_worker(receiver: mpsc::Receiver<NotificationCommand>) {
    while let Ok(command) = receiver.recv() {
        match command {
            NotificationCommand::Show { title, artist } => {
                let mut notification = Notification::new();
                notification
                    .summary(&title)
                    .body(&artist)
                    .timeout(Timeout::Milliseconds(4_000));
                #[cfg(all(unix, not(target_os = "macos")))]
                notification.appname("Metrolist").icon("audio-x-generic");
                if let Err(error) = notification.show() {
                    tracing::warn!(%error, "now-playing notification could not be shown");
                }
            }
            NotificationCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ArtistCredit;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct FakeState {
        commands: Vec<DesktopMediaCommand>,
        metadata: Vec<Option<DesktopTrack>>,
        playback: Vec<(DesktopPlaybackState, Duration)>,
        volumes: Vec<f32>,
        notifications: Vec<String>,
    }

    struct FakeBackend(Arc<Mutex<FakeState>>);

    impl DesktopMediaBackend for FakeBackend {
        fn drain_commands(&mut self) -> Vec<DesktopMediaCommand> {
            std::mem::take(&mut self.0.lock().unwrap().commands)
        }

        fn set_metadata(&mut self, track: &DesktopTrack) -> std::result::Result<(), String> {
            self.0.lock().unwrap().metadata.push(Some(track.clone()));
            Ok(())
        }

        fn clear_metadata(&mut self) -> std::result::Result<(), String> {
            self.0.lock().unwrap().metadata.push(None);
            Ok(())
        }

        fn set_playback(
            &mut self,
            state: DesktopPlaybackState,
            position: Duration,
        ) -> std::result::Result<(), String> {
            self.0.lock().unwrap().playback.push((state, position));
            Ok(())
        }

        fn set_volume(&mut self, volume: f32) -> std::result::Result<(), String> {
            self.0.lock().unwrap().volumes.push(volume);
            Ok(())
        }

        fn show_now_playing(&mut self, track: &DesktopTrack) -> std::result::Result<(), String> {
            self.0.lock().unwrap().notifications.push(track.id.clone());
            Ok(())
        }
    }

    fn fixture_song(id: &str) -> Song {
        Song {
            video_id: id.into(),
            title: "Fixture track".into(),
            artists: vec![ArtistCredit {
                id: None,
                name: "Fixture artist".into(),
            }],
            duration: Some(Duration::from_secs(180)),
            thumbnail_url: Some("https://example.test/cover.jpg".into()),
            album: None,
            is_episode: false,
        }
    }

    #[test]
    fn session_deduplicates_metadata_notifications_and_frequent_progress() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let mut session = DesktopMediaSession::with_backend(Box::new(FakeBackend(state.clone())));
        let song = fixture_song("fixture-one");

        session.sync(Some(&song), PlaybackState::Loading, Duration::ZERO, 0.8);
        session.sync(
            Some(&song),
            PlaybackState::Playing,
            Duration::from_millis(250),
            0.8,
        );
        session.sync(
            Some(&song),
            PlaybackState::Playing,
            Duration::from_millis(500),
            0.8,
        );
        session.sync(
            Some(&song),
            PlaybackState::Playing,
            Duration::from_millis(1_300),
            0.8,
        );

        let state = state.lock().unwrap();
        assert_eq!(state.metadata.len(), 1);
        assert_eq!(state.notifications, ["fixture-one"]);
        assert_eq!(state.volumes, [0.8]);
        assert_eq!(
            state.playback,
            [
                (DesktopPlaybackState::Paused, Duration::ZERO),
                (DesktopPlaybackState::Playing, Duration::from_millis(250)),
                (DesktopPlaybackState::Playing, Duration::from_millis(1_300)),
            ]
        );
    }

    #[test]
    fn session_clears_metadata_and_forwards_commands() {
        let state = Arc::new(Mutex::new(FakeState {
            commands: vec![
                DesktopMediaCommand::Previous,
                DesktopMediaCommand::SetPosition(Duration::from_secs(42)),
            ],
            ..FakeState::default()
        }));
        let mut session = DesktopMediaSession::with_backend(Box::new(FakeBackend(state.clone())));
        session.sync(
            Some(&fixture_song("fixture-two")),
            PlaybackState::Paused,
            Duration::ZERO,
            0.5,
        );
        session.sync(None, PlaybackState::Idle, Duration::ZERO, 0.5);

        assert_eq!(
            session.drain_commands(),
            [
                DesktopMediaCommand::Previous,
                DesktopMediaCommand::SetPosition(Duration::from_secs(42)),
            ]
        );
        let state = state.lock().unwrap();
        assert_eq!(state.metadata.last(), Some(&None));
        assert_eq!(
            state.playback.last(),
            Some(&(DesktopPlaybackState::Stopped, Duration::ZERO))
        );
    }

    #[test]
    fn souvlaki_events_map_to_bounded_application_commands() {
        assert_eq!(
            map_media_control_event(MediaControlEvent::Seek(SeekDirection::Backward)),
            Some(DesktopMediaCommand::SeekRelative {
                direction: DesktopSeekDirection::Backward,
                amount: Duration::from_secs(10),
            })
        );
        assert_eq!(
            map_media_control_event(MediaControlEvent::SetVolume(1.5)),
            Some(DesktopMediaCommand::SetVolume(1.0))
        );
        assert_eq!(
            map_media_control_event(MediaControlEvent::OpenUri("https://example.test".into())),
            None
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    #[test]
    fn unavailable_session_bus_prevents_media_control_startup() {
        let available = session_bus_probe(|| -> std::result::Result<(), &'static str> {
            Err("session bus unavailable")
        });

        assert!(!available);
    }
}
