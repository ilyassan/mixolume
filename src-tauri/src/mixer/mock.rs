//! In-memory fake [`AudioMixerBackend`], used only by tests (and, during early Phase 0
//! frontend work, could be swapped in for `new_platform_backend()` to develop the UI against
//! a realistic data shape without a real OS audio session running).

use std::sync::Mutex;

use super::{clamp_volume, AppSession, AudioMixerBackend, MixerError};

pub struct MockMixerBackend {
    sessions: Mutex<Vec<AppSession>>,
}

impl MockMixerBackend {
    pub fn new(sessions: Vec<AppSession>) -> Self {
        Self {
            sessions: Mutex::new(sessions),
        }
    }
}

impl AudioMixerBackend for MockMixerBackend {
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError> {
        Ok(self.sessions.lock().unwrap().clone())
    }

    fn set_volume(&self, session_id: &str, volume: f32) -> Result<(), MixerError> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .iter_mut()
            .find(|s| s.id == session_id)
            .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;
        session.volume = clamp_volume(volume);
        Ok(())
    }

    fn set_muted(&self, session_id: &str, muted: bool) -> Result<(), MixerError> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .iter_mut()
            .find(|s| s.id == session_id)
            .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;
        session.muted = muted;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session(id: &str) -> AppSession {
        AppSession {
            id: id.to_string(),
            display_name: format!("App {id}"),
            icon_png: None,
            volume: 0.5,
            muted: false,
            is_active: true,
        }
    }

    #[test]
    fn set_volume_clamps_out_of_range_input() {
        let backend = MockMixerBackend::new(vec![sample_session("1")]);
        backend.set_volume("1", 3.0).unwrap();
        assert_eq!(backend.list_sessions().unwrap()[0].volume, 1.0);
    }

    #[test]
    fn set_volume_on_unknown_session_errors() {
        let backend = MockMixerBackend::new(vec![sample_session("1")]);
        let err = backend.set_volume("missing", 0.5).unwrap_err();
        assert!(matches!(err, MixerError::SessionNotFound(id) if id == "missing"));
    }

    #[test]
    fn set_muted_toggles_flag() {
        let backend = MockMixerBackend::new(vec![sample_session("1")]);
        backend.set_muted("1", true).unwrap();
        assert!(backend.list_sessions().unwrap()[0].muted);
    }
}
