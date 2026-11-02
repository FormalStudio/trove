//! One sound at a time: the play button of an audio preview, through nd's audio player
//! (GStreamer on Linux, loaded when it first plays). A new selection or a closed Quick Look
//! stops it.

use std::path::{Path, PathBuf};

use nd::prelude::*;
use nd::system::{AudioEvent, AudioState, PlayOptions, Player, SystemEvent};

pub struct Sound {
    playing: Option<(PathBuf, Player)>,
    paused: bool,
}

struct Shared(Entity<Sound>);

pub fn get(cx: &mut App) -> Entity<Sound> {
    if let Some(shared) = cx.try_global::<Shared>() {
        return shared.0.clone();
    }
    let entity = cx.new(|_| Sound { playing: None, paused: false });
    let weak = entity.downgrade();
    if crate::pump::live() {
        nd::system::on_event(&nd::system::system(), move |event, cx| {
            let SystemEvent::Audio { player, event: AudioEvent::State { state, .. }, .. } = event else { return };
            let (player, state) = (*player, *state);
            let _ = weak.update(cx, |s, cx| s.on_state(player, state, cx));
        });
    }
    cx.set_global(Shared(entity.clone()));
    entity
}

impl Sound {
    /// `Some(true)` while `path` plays, `Some(false)` while it is paused, `None` otherwise.
    pub fn state_of(&self, path: &Path) -> Option<bool> {
        self.playing.as_ref().filter(|(p, _)| p == path).map(|_| !self.paused)
    }

    pub fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        match &self.playing {
            Some((p, player)) if p == path => {
                if self.paused {
                    drop(player.resume());
                } else {
                    drop(player.pause());
                }
                self.paused = !self.paused;
            }
            _ => {
                self.stop(cx);
                let (player, started) = nd::system::system().audio().play(PlayOptions::path(path));
                self.playing = Some((path.to_path_buf(), player.clone()));
                self.paused = false;
                let me = cx.weak_entity();
                let id = player.id();
                nd::system::spawn(started, move |result, cx| {
                    if result.is_err() {
                        let _ = me.update(cx, |s, cx| s.on_state(id, AudioState::Error, cx));
                    }
                });
            }
        }
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some((_, player)) = self.playing.take() {
            drop(player.stop());
            cx.notify();
        }
    }

    fn on_state(&mut self, id: nd::system::PlayerId, state: AudioState, cx: &mut Context<Self>) {
        let ours = self.playing.as_ref().is_some_and(|(_, p)| p.id() == id);
        if !ours {
            return;
        }
        match state {
            AudioState::Ended | AudioState::Stopped | AudioState::Error => self.playing = None,
            AudioState::Paused => self.paused = true,
            AudioState::Playing => self.paused = false,
        }
        cx.notify();
    }
}

/// Stops whatever plays unless it is `keep`.
pub fn stop_unless(keep: Option<&Path>, cx: &mut App) {
    let sound = get(cx);
    sound.update(cx, |s, cx| {
        if s.playing.as_ref().is_some_and(|(p, _)| Some(p.as_path()) != keep) {
            s.stop(cx);
        }
    });
}

/// The play and pause button of an audio preview.
pub fn play_button(path: &Path, playing: bool) -> AnyElement {
    let path = path.to_path_buf();
    let (glyph, label) = if playing { (Icon::Pause, "Pause") } else { (Icon::Play, "Play") };
    button("preview-play", label)
        .icon(glyph)
        .on_click(move |_: &ClickEvent, _, cx| {
            let sound = get(cx);
            sound.update(cx, |s, cx| s.toggle(&path, cx));
        })
        .into_any_element()
}
