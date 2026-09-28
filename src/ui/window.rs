use std::cell::{Cell, RefCell};
use std::rc::Rc;

use async_channel::{Receiver, Sender};
use gtk4::prelude::*;
use libadwaita::prelude::*;
use tracing::{error, info};

use crate::app_state::AppState;
use crate::claude::backend::{Backend, Message};
use crate::claude::backends::claude_code::PermissionRequest;
use crate::claude::ClaudeCodeBackend;
use super::message_row::new_message_row;
use super::waveform::WaveformWidget;

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// Commands sent from the UI to the background worker.
#[derive(Debug, Clone)]
pub enum AppCmd {
    StartListening,
    StopListening,
    Cancel,
    /// End the current session: stops the auto-listen loop and returns the
    /// worker to idle, waiting for the next StartListening command.
    StopSession,
    /// Sent when the window is destroyed so the worker thread can exit cleanly.
    Shutdown,
    /// Response to a tool permission request shown by a GTK dialog.
    PermissionResponse { request_id: String, allow: bool },
}

/// Events sent from the background worker to the UI.
#[derive(Debug, Clone)]
pub enum AppEvent {
    StateChanged(AppState),
    /// Audio samples for the waveform visualiser.
    WaveformSamples(Vec<f32>),
    /// Partial in-flight transcript (user still speaking).
    PartialTranscript(String),
    /// Final transcript; seals the user bubble and triggers AI response.
    /// An empty string means "clear any pending row without creating a bubble".
    NewUserMessage(String),
    /// A text token from the assistant stream — append to current bubble.
    AssistantChunk(String),
    /// The assistant response is complete; seal the bubble.
    AssistantDone,
    /// Claude wants to use a tool; show a permission dialog.
    PermissionRequest(PermissionRequest),
}

pub type AppCmdSender = Sender<AppCmd>;
pub type AppEventSender = Sender<AppEvent>;

// ---------------------------------------------------------------------------
// build_window — public entry point
// ---------------------------------------------------------------------------

pub fn build_window(app: &libadwaita::Application) -> libadwaita::ApplicationWindow {
    let (cmd_tx, cmd_rx) = async_channel::bounded::<AppCmd>(32);
    let (event_tx, event_rx) = async_channel::bounded::<AppEvent>(256);

    // ---- Root window ----
    let win = libadwaita::ApplicationWindow::builder()
        .application(app)
        .title("VoiceChat")
        .default_width(960)
        .default_height(720)
        .build();

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    // ---- Header bar ----
    let header = libadwaita::HeaderBar::new();
    let cfg = crate::config::load_config();
    let preset_name = crate::config::active_preset(&cfg).name;
    let title_widget = libadwaita::WindowTitle::builder()
        .title("VoiceChat")
        .subtitle(&preset_name)
        .build();
    header.set_title_widget(Some(&title_widget));
    let gear_btn = gtk4::Button::from_icon_name("emblem-system-symbolic");
    gear_btn.set_tooltip_text(Some("Settings (future use)"));
    header.pack_end(&gear_btn);
    vbox.append(&header);

    // ---- Conversation list ----
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_hexpand(true);
    scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    let listbox = gtk4::ListBox::new();
    listbox.set_selection_mode(gtk4::SelectionMode::None);
    scrolled.set_child(Some(&listbox));
    vbox.append(&scrolled);

    // ---- Waveform widget ----
    let waveform = WaveformWidget::new();
    vbox.append(waveform.widget());

    // ---- Status row: spinner + label ----
    let status_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    status_row.set_margin_start(12);
    status_row.set_margin_end(12);
    status_row.set_margin_top(4);
    status_row.set_margin_bottom(4);
    let spinner = gtk4::Spinner::new();
    let status_label = gtk4::Label::new(Some("Ready"));
    status_label.set_hexpand(true);
    status_label.set_xalign(0.0);
    status_row.append(&spinner);
    status_row.append(&status_label);
    vbox.append(&status_row);

    // ---- Bottom bar: session button + cancel button ----
    let input_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    input_row.set_margin_start(12);
    input_row.set_margin_end(12);
    input_row.set_margin_top(4);
    input_row.set_margin_bottom(8);
    input_row.set_halign(gtk4::Align::Center);

    let session_button = gtk4::Button::builder()
        .label("Start Conversation")
        .tooltip_text("Start or end a voice session")
        .build();
    session_button.add_css_class("suggested-action");
    session_button.add_css_class("pill");

    let cancel_button = gtk4::Button::builder()
        .icon_name("process-stop-symbolic")
        .tooltip_text("Cancel / interrupt current turn")
        .visible(false)
        .build();
    cancel_button.add_css_class("destructive-action");
    cancel_button.add_css_class("circular");

    input_row.append(&session_button);
    input_row.append(&cancel_button);
    vbox.append(&input_row);

    win.set_content(Some(&vbox));

    // ---------------------------------------------------------------------------
    // Shared UI state
    // ---------------------------------------------------------------------------

    let in_session: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    // Current assistant label being streamed into, plus its text accumulator.
    let current_asst_label: Rc<RefCell<Option<gtk4::Label>>> = Rc::new(RefCell::new(None));
    let asst_text_acc: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    // Pending user row (shown while partial transcription is in progress).
    let pending_user_row: Rc<RefCell<Option<gtk4::ListBoxRow>>> =
        Rc::new(RefCell::new(None));
    // Label inside the pending user row, kept separate so we can update text.
    let pending_user_label: Rc<RefCell<Option<gtk4::Label>>> =
        Rc::new(RefCell::new(None));

    // ---------------------------------------------------------------------------
    // Session button signal
    // ---------------------------------------------------------------------------
    {
        let cmd_tx = cmd_tx.clone();
        let in_session = in_session.clone();
        let session_button = session_button.clone();

        session_button.connect_clicked(move |btn| {
            if in_session.get() {
                // End session
                let tx = cmd_tx.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(AppCmd::Cancel).await;
                    let _ = tx.send(AppCmd::StopSession).await;
                });
                in_session.set(false);
                btn.set_label("Start Conversation");
                btn.remove_css_class("destructive-action");
                btn.add_css_class("suggested-action");
            } else {
                // Start session
                let tx = cmd_tx.clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = tx.send(AppCmd::StartListening).await {
                        error!("Failed to send StartListening: {e}");
                    }
                });
                in_session.set(true);
                btn.set_label("End Conversation");
                btn.remove_css_class("suggested-action");
                btn.add_css_class("destructive-action");
            }
        });
    }

    // ---------------------------------------------------------------------------
    // Cancel button signal
    // ---------------------------------------------------------------------------
    {
        let cmd_tx = cmd_tx.clone();
        cancel_button.connect_clicked(move |_| {
            let tx = cmd_tx.clone();
            glib::spawn_future_local(async move {
                if let Err(e) = tx.send(AppCmd::Cancel).await {
                    error!("Failed to send Cancel: {e}");
                }
            });
        });
    }

    // ---------------------------------------------------------------------------
    // Event loop (GTK main thread)
    // ---------------------------------------------------------------------------
    {
        let waveform_widget = waveform;
        let spinner = spinner.clone();
        let status_label = status_label.clone();
        let cancel_button = cancel_button.clone();
        let session_button_ev = session_button.clone();
        let in_session_ev = in_session.clone();
        let listbox = listbox.clone();
        let scrolled = scrolled.clone();
        let current_asst_label = current_asst_label.clone();
        let asst_text_acc = asst_text_acc.clone();
        let pending_user_row = pending_user_row.clone();
        let pending_user_label = pending_user_label.clone();
        let win_ev = win.clone();
        let cmd_tx_ev = cmd_tx.clone();

        glib::spawn_future_local(async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    AppEvent::StateChanged(state) => {
                        if let AppState::Error(ref msg) = state {
                            status_label.set_text(msg);
                        } else {
                            status_label.set_text(state.status_label());
                        }
                        if state.show_spinner() {
                            spinner.start();
                        } else {
                            spinner.stop();
                        }
                        waveform_widget.widget().set_visible(state.show_waveform());
                        cancel_button.set_visible(state.show_cancel());

                        // When the worker returns to Idle/Error, reset the
                        // session button to "Start Conversation".
                        if matches!(state, AppState::Idle | AppState::Error(_)) {
                            in_session_ev.set(false);
                            session_button_ev.set_label("Start Conversation");
                            session_button_ev.remove_css_class("destructive-action");
                            session_button_ev.add_css_class("suggested-action");
                        }
                    }

                    AppEvent::WaveformSamples(samples) => {
                        waveform_widget.push_samples(&samples);
                    }

                    AppEvent::PartialTranscript(text) => {
                        let has_pending = pending_user_row.borrow().is_some();
                        if has_pending {
                            if let Some(ref lbl) = *pending_user_label.borrow() {
                                lbl.set_text(&text);
                            }
                        } else {
                            let (row_widget, lbl) = new_message_row("user", &text);
                            lbl.add_css_class("dim-label");
                            let list_row = gtk4::ListBoxRow::new();
                            list_row.set_child(Some(&row_widget));
                            list_row.set_activatable(false);
                            listbox.append(&list_row);
                            *pending_user_row.borrow_mut() = Some(list_row);
                            *pending_user_label.borrow_mut() = Some(lbl);
                            scroll_to_bottom(&scrolled);
                        }
                    }

                    AppEvent::NewUserMessage(text) => {
                        if text.is_empty() {
                            // Cancel with no speech: remove the pending row if any
                            if let Some(ref row) = *pending_user_row.borrow() {
                                row.unparent();
                            }
                            *pending_user_row.borrow_mut() = None;
                            *pending_user_label.borrow_mut() = None;
                        } else if pending_user_row.borrow().is_some() {
                            // Seal the pending row with the final text
                            if let Some(ref lbl) = *pending_user_label.borrow() {
                                lbl.set_text(&text);
                                lbl.remove_css_class("dim-label");
                            }
                            *pending_user_row.borrow_mut() = None;
                            *pending_user_label.borrow_mut() = None;
                            scroll_to_bottom(&scrolled);
                        } else {
                            // No pending row (partial transcription wasn't started)
                            let (row_widget, _lbl) = new_message_row("user", &text);
                            let list_row = gtk4::ListBoxRow::new();
                            list_row.set_child(Some(&row_widget));
                            list_row.set_activatable(false);
                            listbox.append(&list_row);
                            scroll_to_bottom(&scrolled);
                        }
                    }

                    AppEvent::AssistantChunk(chunk) => {
                        let label = {
                            let mut label_ref = current_asst_label.borrow_mut();
                            if let Some(ref lbl) = *label_ref {
                                lbl.clone()
                            } else {
                                let (row_widget, lbl) = new_message_row("assistant", "");
                                let list_row = gtk4::ListBoxRow::new();
                                list_row.set_child(Some(&row_widget));
                                list_row.set_activatable(false);
                                listbox.append(&list_row);
                                *label_ref = Some(lbl.clone());
                                lbl
                            }
                        };
                        {
                            let mut acc = asst_text_acc.borrow_mut();
                            acc.push_str(&chunk);
                            label.set_text(&*acc);
                        }
                        scroll_to_bottom(&scrolled);
                    }

                    AppEvent::AssistantDone => {
                        *current_asst_label.borrow_mut() = None;
                        asst_text_acc.borrow_mut().clear();
                    }

                    AppEvent::PermissionRequest(req) => {
                        let description = format!(
                            "Allow Claude to use {}?\n\n{}",
                            req.tool_name, req.input_summary
                        );
                        let dialog = libadwaita::AlertDialog::new(
                            Some("Tool Permission Request"),
                            Some(&description),
                        );
                        dialog.add_response("deny", "Deny");
                        dialog.add_response("allow", "Allow");
                        dialog.set_response_appearance(
                            "allow",
                            libadwaita::ResponseAppearance::Suggested,
                        );
                        dialog.set_response_appearance(
                            "deny",
                            libadwaita::ResponseAppearance::Destructive,
                        );
                        let cmd_tx_dialog = cmd_tx_ev.clone();
                        let request_id = req.request_id.clone();
                        dialog.connect_response(None, move |_, response| {
                            let allow = response == "allow";
                            let tx = cmd_tx_dialog.clone();
                            let id = request_id.clone();
                            glib::spawn_future_local(async move {
                                let _ = tx
                                    .send(AppCmd::PermissionResponse {
                                        request_id: id,
                                        allow,
                                    })
                                    .await;
                            });
                        });
                        dialog.present(Some(&win_ev));
                    }
                }
            }
        });
    }

    // ---------------------------------------------------------------------------
    // Window close → Shutdown
    // ---------------------------------------------------------------------------
    {
        let cmd_tx = cmd_tx.clone();
        win.connect_destroy(move |_| {
            let _ = cmd_tx.try_send(AppCmd::Shutdown);
        });
    }

    spawn_worker(cmd_rx, event_tx);
    win
}

// ---------------------------------------------------------------------------
// UI helper
// ---------------------------------------------------------------------------

fn scroll_to_bottom(scrolled: &gtk4::ScrolledWindow) {
    let adj = scrolled.vadjustment();
    glib::idle_add_local_once(move || {
        adj.set_value(adj.upper() - adj.page_size());
    });
}

// ---------------------------------------------------------------------------
// Worker bootstrap
// ---------------------------------------------------------------------------

fn spawn_worker(cmd_rx: Receiver<AppCmd>, event_tx: Sender<AppEvent>) {
    std::thread::spawn(move || {
        // Use a current-thread runtime + LocalSet so that spawn_local tasks
        // (e.g. the TTS worker) can capture non-Send types like AudioPlayback.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, run_worker(cmd_rx, event_tx));
    });
}

// ---------------------------------------------------------------------------
// Background worker
// ---------------------------------------------------------------------------

async fn run_worker(cmd_rx: Receiver<AppCmd>, event_tx: Sender<AppEvent>) {
    use crate::{audio, config, stt, tts};
    use futures::StreamExt;

    let _ = event_tx.send(AppEvent::StateChanged(AppState::Idle)).await;

    // ---- Ensure Silero VAD model ----
    let vad_model_path = match audio::vad_model::ensure_model().await {
        Ok(p) => p,
        Err(e) => {
            let _ = event_tx
                .send(AppEvent::StateChanged(AppState::Error(format!(
                    "VAD model error: {e}"
                ))))
                .await;
            return;
        }
    };

    // ---- Initialise SileroVad ----
    let mut silero_vad = match audio::SileroVad::new(&vad_model_path) {
        Ok(v) => v,
        Err(e) => {
            let _ = event_tx
                .send(AppEvent::StateChanged(AppState::Error(format!(
                    "VAD init error: {e}"
                ))))
                .await;
            return;
        }
    };

    // ---- Ensure Whisper model ----
    let whisper_model_path = match stt::model::ensure_model().await {
        Ok(p) => p,
        Err(e) => {
            let _ = event_tx
                .send(AppEvent::StateChanged(AppState::Error(format!(
                    "Whisper model error: {e}"
                ))))
                .await;
            return;
        }
    };

    // ---- Initialise Whisper ----
    let whisper = match stt::WhisperStt::new(&whisper_model_path) {
        Ok(w) => w,
        Err(e) => {
            let _ = event_tx
                .send(AppEvent::StateChanged(AppState::Error(format!(
                    "Whisper init failed: {e}"
                ))))
                .await;
            return;
        }
    };

    // ---- Initialise Kokoro TTS (optional) ----
    let tts_model = config::tts_model_path();
    let tts_voices = config::tts_voices_path();
    let kokoro = tts::KokoroTts::new(&tts_model, &tts_voices).await.ok();
    if kokoro.is_none() {
        info!("TTS model not found — text-only mode");
    }

    // ---- Audio playback ----
    let playback = audio::AudioPlayback::new();
    if let Err(e) = playback.start() {
        error!("Playback init failed: {e}");
    }

    // ---- ClaudeCode backend ----
    let backend = ClaudeCodeBackend::new();
    let (perm_tx, mut perm_rx) =
        tokio::sync::mpsc::unbounded_channel::<PermissionRequest>();
    backend.set_permission_channel(perm_tx);
    let mut history: Vec<Message> = Vec::new();

    // ---- Start audio capture once (always-on mic) ----
    let capture = audio::AudioCapture::new();
    let mut audio_rx = match capture.start() {
        Ok(r) => r,
        Err(e) => {
            let _ = event_tx
                .send(AppEvent::StateChanged(AppState::Error(format!(
                    "Audio capture error: {e}"
                ))))
                .await;
            return;
        }
    };

    // =========================================================================
    // State machine
    // =========================================================================
    enum AgentState {
        Idle,
        Listening,
        Transcribing,
        Responding {
            claude_handle: tokio::task::JoinHandle<()>,
            tts_handle: tokio::task::JoinHandle<()>,
        },
    }

    const SPEECH_PROB_THRESHOLD: f32 = 0.5;
    const SILENCE_PROB_THRESHOLD: f32 = 0.35;
    const SILENCE_DURATION_MS: u64 = 300;
    const INTERRUPT_PROB_THRESHOLD: f32 = 0.7;
    const INTERRUPT_DURATION_MS: u64 = 400;
    const VAD_CHUNK_SAMPLES: usize = 512;
    const MIN_STT_SAMPLES: usize = 16_480; // ~1030 ms at 16 kHz — Whisper needs > 1000 ms
    const POST_TTS_SUPPRESSION_CHUNKS: u32 = 25; // ~800ms suppression after TTS ends

    let mut agent_state = AgentState::Idle;
    let mut ring_buf: Vec<f32> = Vec::new();

    // Variables for Listening / Transcribing
    let mut speech_buf: Vec<f32> = Vec::new();
    let mut was_speaking = false;
    let mut silence_ms_counter: u64 = 0;

    // Counter for periodic VAD debug logging
    let mut chunk_count: u64 = 0;

    // STT join handle + extra speech accumulated during Transcribing
    let mut stt_handle: Option<tokio::task::JoinHandle<anyhow::Result<String>>> = None;
    let mut stt_extra_speech: Vec<f32> = Vec::new();

    // Interrupt counter for Responding
    let mut interrupt_ms_counter: u64 = 0;

    // Suppression window after TTS ends to prevent speaker-bleed false triggers
    let mut suppression_chunks_remaining: u32 = 0;

    // Shared accumulator so we can add the assistant turn to history on completion
    let mut response_acc: Option<std::sync::Arc<std::sync::Mutex<String>>> = None;

    // =========================================================================
    // Main loop
    // =========================================================================
    'main: loop {
        // ------------------------------------------------------------------
        // Step 1 — drain command channel (non-blocking)
        // ------------------------------------------------------------------
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                AppCmd::Shutdown => return,

                AppCmd::PermissionResponse { request_id, allow } => {
                    backend.respond_to_permission(request_id, allow);
                }

                AppCmd::StartListening => {
                    if matches!(agent_state, AgentState::Idle) {
                        silero_vad.reset();
                        speech_buf.clear();
                        was_speaking = false;
                        silence_ms_counter = 0;
                        agent_state = AgentState::Listening;
                        let _ = event_tx
                            .send(AppEvent::StateChanged(AppState::Listening))
                            .await;
                    }
                }

                AppCmd::Cancel | AppCmd::StopListening | AppCmd::StopSession => {
                    // Abort any running handles
                    if let AgentState::Responding {
                        ref claude_handle,
                        ref tts_handle,
                    } = agent_state
                    {
                        claude_handle.abort();
                        tts_handle.abort();
                        playback.clear();
                    }
                    if matches!(agent_state, AgentState::Transcribing) {
                        if let Some(ref h) = stt_handle {
                            h.abort();
                        }
                        stt_handle = None;
                    }
                    // Commit any partial assistant response to history
                    if let Some(ref acc) = response_acc {
                        let resp = acc.lock().unwrap().clone();
                        if !resp.is_empty() {
                            history.push(Message {
                                role: "assistant".into(),
                                content: resp,
                            });
                        }
                    }
                    response_acc = None;
                    speech_buf.clear();
                    stt_extra_speech.clear();
                    was_speaking = false;
                    silence_ms_counter = 0;
                    interrupt_ms_counter = 0;
                    agent_state = AgentState::Idle;
                    let _ = event_tx
                        .send(AppEvent::NewUserMessage(String::new()))
                        .await;
                    let _ = event_tx
                        .send(AppEvent::StateChanged(AppState::Idle))
                        .await;
                }
            }
        }

        // Drain permission requests from the backend → forward to UI
        while let Ok(req) = perm_rx.try_recv() {
            let _ = event_tx.try_send(AppEvent::PermissionRequest(req));
        }

        // ------------------------------------------------------------------
        // Step 2 — read next audio chunk (blocks until data arrives)
        // ------------------------------------------------------------------
        let chunk = match audio_rx.recv().await {
            Some(c) => c,
            None => {
                let _ = event_tx
                    .send(AppEvent::StateChanged(AppState::Error(
                        "Audio capture ended unexpectedly".into(),
                    )))
                    .await;
                break 'main;
            }
        };

        // ------------------------------------------------------------------
        // Step 3 — send waveform samples to UI
        // ------------------------------------------------------------------
        let _ = event_tx.try_send(AppEvent::WaveformSamples(chunk.clone()));

        // ------------------------------------------------------------------
        // Step 4 — accumulate into 512-sample ring buffer
        // ------------------------------------------------------------------
        ring_buf.extend_from_slice(&chunk);

        // ------------------------------------------------------------------
        // Steps 5 & 6 — drain in 512-sample windows; run VAD + state machine
        // ------------------------------------------------------------------
        while ring_buf.len() >= VAD_CHUNK_SAMPLES {
            let window: [f32; VAD_CHUNK_SAMPLES] =
                ring_buf[..VAD_CHUNK_SAMPLES].try_into().unwrap();
            ring_buf.drain(..VAD_CHUNK_SAMPLES);

            let prob = match silero_vad.process(&window) {
                Ok(p) => p,
                Err(e) => {
                    error!("VAD process error: {e}");
                    0.0
                }
            };

            // ---- Idle: discard ----
            if matches!(agent_state, AgentState::Idle) {
                continue;
            }

            // ---- Listening ----
            if matches!(agent_state, AgentState::Listening) {
                // Post-TTS suppression: skip VAD for a brief window after TTS ends
                // to avoid speaker-bleed and LSTM warm-up false triggers.
                if suppression_chunks_remaining > 0 {
                    suppression_chunks_remaining -= 1;
                    continue;
                }

                chunk_count += 1;
                if chunk_count % 50 == 0 {
                    info!(
                        "VAD prob: {:.3}, speech_buf len: {}, was_speaking: {}",
                        prob,
                        speech_buf.len(),
                        was_speaking
                    );
                }

                if prob >= SPEECH_PROB_THRESHOLD {
                    speech_buf.extend_from_slice(&window);
                    was_speaking = true;
                    silence_ms_counter = 0;
                } else if prob < SILENCE_PROB_THRESHOLD && was_speaking {
                    if silence_ms_counter >= SILENCE_DURATION_MS {
                        // Ensure we have enough audio for Whisper (≥ 1 second)
                        if speech_buf.len() < MIN_STT_SAMPLES {
                            speech_buf.resize(MIN_STT_SAMPLES, 0.0_f32);
                        }
                        // Enough silence after speech → kick off STT
                        info!(
                            "End of utterance: {} samples → spawning STT",
                            speech_buf.len()
                        );
                        let speech = speech_buf.clone();
                        let w = whisper.clone();
                        stt_handle = Some(tokio::task::spawn_blocking(move || {
                            w.transcribe(speech)
                        }));
                        speech_buf.clear();
                        was_speaking = false;
                        silence_ms_counter = 0;
                        stt_extra_speech.clear();
                        agent_state = AgentState::Transcribing;
                        let _ = event_tx
                            .try_send(AppEvent::StateChanged(AppState::Transcribing));
                    } else {
                        silence_ms_counter += 32; // 512 samples @ 16 kHz = 32 ms
                    }
                }
            }

            // ---- Transcribing ----
            if matches!(agent_state, AgentState::Transcribing) {
                // Keep accumulating speech while we wait
                if prob >= SPEECH_PROB_THRESHOLD {
                    stt_extra_speech.extend_from_slice(&window);
                }

                // Poll STT handle (non-blocking)
                let stt_done = stt_handle.as_ref().map_or(false, |h| h.is_finished());
                if stt_done {
                    let result = stt_handle.take().unwrap().await;
                    let transcript = match result {
                        Ok(Ok(t)) => t,
                        Ok(Err(e)) => {
                            error!("STT error: {e}");
                            let _ = event_tx
                                .try_send(AppEvent::NewUserMessage(String::new()));
                            speech_buf =
                                std::mem::take(&mut stt_extra_speech);
                            was_speaking = false;
                            silence_ms_counter = 0;
                            agent_state = AgentState::Listening;
                            let _ = event_tx
                                .try_send(AppEvent::StateChanged(AppState::Listening));
                            continue;
                        }
                        Err(e) => {
                            error!("STT task panicked: {e}");
                            let _ = event_tx
                                .try_send(AppEvent::NewUserMessage(String::new()));
                            speech_buf =
                                std::mem::take(&mut stt_extra_speech);
                            was_speaking = false;
                            silence_ms_counter = 0;
                            agent_state = AgentState::Listening;
                            let _ = event_tx
                                .try_send(AppEvent::StateChanged(AppState::Listening));
                            continue;
                        }
                    };

                    // Discard blank/noise transcripts from Whisper
                    if is_blank_transcript(&transcript) {
                        info!("Blank transcript discarded, returning to Listening");
                        silero_vad.reset();
                        speech_buf.clear();
                        was_speaking = false;
                        silence_ms_counter = 0;
                        agent_state = AgentState::Listening;
                        let _ = event_tx
                            .try_send(AppEvent::StateChanged(AppState::Listening));
                        continue;
                    }

                    if transcript.is_empty() {
                        // Nothing heard — go back to listening
                        let _ = event_tx
                            .try_send(AppEvent::NewUserMessage(String::new()));
                        speech_buf = std::mem::take(&mut stt_extra_speech);
                        was_speaking = false;
                        silence_ms_counter = 0;
                        agent_state = AgentState::Listening;
                        let _ = event_tx
                            .try_send(AppEvent::StateChanged(AppState::Listening));
                        continue;
                    }

                    // Good transcript → start Claude response
                    let _ = event_tx
                        .try_send(AppEvent::NewUserMessage(transcript.clone()));
                    let _ = event_tx
                        .try_send(AppEvent::StateChanged(AppState::Thinking));

                    // stream_message uses history *before* this turn
                    let stream = match backend.stream_message(&history, &transcript) {
                        Ok(s) => s,
                        Err(e) => {
                            error!("Backend error: {e}");
                            let _ = event_tx.try_send(AppEvent::StateChanged(
                                AppState::Error(format!("Backend error: {e}")),
                            ));
                            speech_buf = std::mem::take(&mut stt_extra_speech);
                            was_speaking = false;
                            silence_ms_counter = 0;
                            agent_state = AgentState::Listening;
                            continue;
                        }
                    };

                    // Add user turn to history
                    history.push(Message {
                        role: "user".into(),
                        content: transcript.clone(),
                    });

                    // TTS channel
                    let (tts_tx, mut tts_rx) =
                        tokio::sync::mpsc::channel::<String>(4);

                    // ---- TTS handle (spawn_local: AudioPlayback is !Send) ----
                    let pb = playback.clone();
                    let etx_tts = event_tx.clone();
                    let kokoro_opt = kokoro.clone();
                    let tts_handle = tokio::task::spawn_local(async move {
                        let mut first_sentence = true;
                        while let Some(sentence) = tts_rx.recv().await {
                            if let Some(ref tts) = kokoro_opt {
                                if first_sentence {
                                    first_sentence = false;
                                    let _ = etx_tts
                                        .send(AppEvent::StateChanged(AppState::Speaking))
                                        .await;
                                }
                                let k = tts.clone();
                                let stripped = strip_markdown(&sentence);
                                let s = {
                                    let t = stripped.trim_end();
                                    if t.ends_with(['.', '!', '?', ',', ';', ':']) {
                                        stripped
                                    } else {
                                        format!("{},", t)
                                    }
                                };
                                match tokio::task::spawn_blocking(move || {
                                    k.synthesize(&s)
                                })
                                .await
                                {
                                    Ok(Ok(samples)) => pb.queue(samples),
                                    Ok(Err(e)) => error!("TTS synthesis error: {e}"),
                                    Err(e) => error!("TTS blocking task error: {e}"),
                                }
                            }
                        }
                    });

                    // ---- Claude handle (spawn_local: stream is Send but kept local) ----
                    let etx_claude = event_tx.clone();
                    let acc =
                        std::sync::Arc::new(std::sync::Mutex::new(String::new()));
                    response_acc = Some(acc.clone());
                    let claude_handle = tokio::task::spawn_local(async move {
                        let mut stream = stream;
                        let mut splitter = SentenceSplitter::new();
                        while let Some(result) = stream.next().await {
                            match result {
                                Ok(text) => {
                                    {
                                        acc.lock().unwrap().push_str(&text);
                                    }
                                    let _ = etx_claude
                                        .try_send(AppEvent::AssistantChunk(text.clone()));
                                    for sentence in splitter.push(&text) {
                                        if !sentence.trim().is_empty() {
                                            let _ = tts_tx.send(sentence).await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Claude stream error: {e}");
                                    break;
                                }
                            }
                        }
                        for sentence in splitter.flush() {
                            if !sentence.trim().is_empty() {
                                let _ = tts_tx.send(sentence).await;
                            }
                        }
                        drop(tts_tx);
                        let _ = etx_claude
                            .send(AppEvent::AssistantDone)
                            .await;
                    });

                    interrupt_ms_counter = 0;
                    // Any speech accumulated while transcribing becomes the start of the next listen window
                    speech_buf = std::mem::take(&mut stt_extra_speech);
                    was_speaking = false;
                    silence_ms_counter = 0;
                    agent_state = AgentState::Responding {
                        claude_handle,
                        tts_handle,
                    };
                }
            }

            // ---- Responding ----
            if matches!(agent_state, AgentState::Responding { .. }) {
                // Interrupt detection: sustained speech above threshold
                let do_interrupt = if let AgentState::Responding {
                    ref claude_handle,
                    ref tts_handle,
                } = agent_state
                {
                    if prob >= INTERRUPT_PROB_THRESHOLD {
                        interrupt_ms_counter += 32;
                        if interrupt_ms_counter >= INTERRUPT_DURATION_MS {
                            claude_handle.abort();
                            tts_handle.abort();
                            true
                        } else {
                            false
                        }
                    } else {
                        interrupt_ms_counter = 0;
                        false
                    }
                } else {
                    false
                };

                if do_interrupt {
                    playback.clear();
                    // Commit partial response to history
                    if let Some(ref acc) = response_acc {
                        let resp = acc.lock().unwrap().clone();
                        if !resp.is_empty() {
                            history.push(Message {
                                role: "assistant".into(),
                                content: resp,
                            });
                        }
                    }
                    response_acc = None;
                    info!("Responding interrupted by user → Listening");
                    silero_vad.reset();
                    suppression_chunks_remaining = POST_TTS_SUPPRESSION_CHUNKS;
                    chunk_count = 0;
                    interrupt_ms_counter = 0;
                    speech_buf.clear();
                    speech_buf.extend_from_slice(&window);
                    was_speaking = true;
                    silence_ms_counter = 0;
                    agent_state = AgentState::Listening;
                    let _ = event_tx
                        .try_send(AppEvent::StateChanged(AppState::Listening));
                    continue;
                }

                // Check for natural completion
                let both_done = if let AgentState::Responding {
                    ref claude_handle,
                    ref tts_handle,
                } = agent_state
                {
                    claude_handle.is_finished() && tts_handle.is_finished()
                } else {
                    false
                };

                if both_done {
                    // Commit response to history
                    if let Some(ref acc) = response_acc {
                        let resp = acc.lock().unwrap().clone();
                        if !resp.is_empty() {
                            history.push(Message {
                                role: "assistant".into(),
                                content: resp,
                            });
                        }
                    }
                    response_acc = None;
                    info!("Responding done naturally → Listening");
                    silero_vad.reset();
                    suppression_chunks_remaining = POST_TTS_SUPPRESSION_CHUNKS;
                    chunk_count = 0;
                    speech_buf.clear();
                    was_speaking = false;
                    silence_ms_counter = 0;
                    interrupt_ms_counter = 0;
                    agent_state = AgentState::Listening;
                    let _ = event_tx
                        .try_send(AppEvent::StateChanged(AppState::Listening));
                }
            }
        } // window loop
    } // 'main
}

// ---------------------------------------------------------------------------
// Blank transcript filter — discards Whisper noise tokens
// ---------------------------------------------------------------------------

/// Returns `true` if the transcript string is a Whisper noise/blank token
/// that should be discarded rather than sent to the backend.
fn is_blank_transcript(s: &str) -> bool {
    let trimmed = s.trim();
    trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("[blank_audio]")
        || trimmed.eq_ignore_ascii_case("[ silence ]")
        || trimmed.eq_ignore_ascii_case("(silence)")
        || trimmed.eq_ignore_ascii_case("[silence]")
        || (trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() < 30)
}

// ---------------------------------------------------------------------------
// Markdown stripper — removes formatting before TTS synthesis
// ---------------------------------------------------------------------------

/// Strip common markdown formatting characters from `text` so that
/// Kokoro synthesizes clean speech.  The original text is preserved
/// elsewhere (e.g. chat bubbles); this copy is only used for TTS.
fn strip_markdown(text: &str) -> String {
    // We apply a series of regex-free transformations in order.
    let mut s = text.to_string();

    // Fenced code blocks: ```...``` — strip the fence markers, keep content.
    while let (Some(open), Some(close)) = (
        s.find("```"),
        s.find("```").and_then(|p| s[p + 3..].find("```").map(|q| p + 3 + q)),
    ) {
        if open == close {
            break; // only one fence found, nothing to pair
        }
        // content is between open+3 and close
        let content = s[open + 3..close].trim().to_string();
        s = format!("{}{}{}", &s[..open], content, &s[close + 3..]);
    }

    // Inline code: `code` → code
    while let Some(open) = s.find('`') {
        if let Some(rel_close) = s[open + 1..].find('`') {
            let close = open + 1 + rel_close;
            let content = s[open + 1..close].to_string();
            s = format!("{}{}{}", &s[..open], content, &s[close + 1..]);
        } else {
            break;
        }
    }

    // Links: [text](url) → text
    loop {
        if let Some(bracket_open) = s.find('[') {
            if let Some(rel_bracket_close) = s[bracket_open + 1..].find(']') {
                let bracket_close = bracket_open + 1 + rel_bracket_close;
                // Check that immediately after ']' there is '('
                if s.as_bytes().get(bracket_close + 1) == Some(&b'(') {
                    if let Some(rel_paren_close) = s[bracket_close + 2..].find(')') {
                        let paren_close = bracket_close + 2 + rel_paren_close;
                        let link_text = s[bracket_open + 1..bracket_close].to_string();
                        s = format!(
                            "{}{}{}",
                            &s[..bracket_open],
                            link_text,
                            &s[paren_close + 1..]
                        );
                        continue;
                    }
                }
            }
        }
        break;
    }

    // Bold/italic combined: ***text*** or ___text___
    for marker in &["***", "___"] {
        loop {
            if let Some(open) = s.find(marker) {
                if let Some(rel_close) = s[open + marker.len()..].find(marker) {
                    let close = open + marker.len() + rel_close;
                    let content = s[open + marker.len()..close].to_string();
                    s = format!("{}{}{}", &s[..open], content, &s[close + marker.len()..]);
                    continue;
                }
            }
            break;
        }
    }

    // Bold: **text** or __text__
    for marker in &["**", "__"] {
        loop {
            if let Some(open) = s.find(marker) {
                if let Some(rel_close) = s[open + marker.len()..].find(marker) {
                    let close = open + marker.len() + rel_close;
                    let content = s[open + marker.len()..close].to_string();
                    s = format!("{}{}{}", &s[..open], content, &s[close + marker.len()..]);
                    continue;
                }
            }
            break;
        }
    }

    // Italic: *text* or _text_
    for marker in &["*", "_"] {
        loop {
            if let Some(open) = s.find(marker) {
                if let Some(rel_close) = s[open + marker.len()..].find(marker) {
                    let close = open + marker.len() + rel_close;
                    let content = s[open + marker.len()..close].to_string();
                    s = format!("{}{}{}", &s[..open], content, &s[close + marker.len()..]);
                    continue;
                }
            }
            break;
        }
    }

    // Strikethrough: ~~text~~
    loop {
        if let Some(open) = s.find("~~") {
            if let Some(rel_close) = s[open + 2..].find("~~") {
                let close = open + 2 + rel_close;
                let content = s[open + 2..close].to_string();
                s = format!("{}{}{}", &s[..open], content, &s[close + 2..]);
                continue;
            }
        }
        break;
    }

    // ATX headings: # / ## / ### at start of a line → strip hashes and space.
    let s = s
        .lines()
        .map(|line| {
            let trimmed = line.trim_start_matches('#');
            if trimmed.len() < line.len() {
                // At least one '#' was removed.
                trimmed.trim_start_matches(' ')
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Blockquotes: '> ' at start of line → strip the prefix.
    let s = s
        .lines()
        .map(|line| {
            if line.starts_with("> ") {
                &line[2..]
            } else if line.starts_with('>') {
                &line[1..]
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    s
}

// ---------------------------------------------------------------------------
// Sentence splitter
// ---------------------------------------------------------------------------

/// Abbreviations after which a period should not end a sentence.
const ABBREVS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "vs", "etc", "fig", "no",
    "i.e", "e.g", "approx", "dept", "est",
    "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Minimum words since the last split before a comma/semicolon/colon triggers a new chunk.
const MIN_WORDS_BEFORE_CLAUSE_SPLIT: usize = 8;

/// Accumulates streaming text tokens and yields complete sentences.
///
/// Uses a char-scan approach to split on sentence-ending punctuation,
/// paragraph breaks, and long comma-separated clauses.  No word is
/// held back between calls — streaming tokens from `claude` are
/// complete words, so the extra delay is unnecessary.
struct SentenceSplitter {
    buffer: String,
}

impl SentenceSplitter {
    fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Push a new text chunk into the buffer and return any newly complete sentences.
    fn push(&mut self, text: &str) -> Vec<String> {
        self.buffer.push_str(text);
        self.drain(false)
    }

    /// Flush everything remaining in the buffer as a final sentence (if non-empty).
    fn flush(&mut self) -> Vec<String> {
        self.drain(true)
    }

    fn drain(&mut self, flush_all: bool) -> Vec<String> {
        let mut sentences: Vec<String> = Vec::new();
        let buf = std::mem::take(&mut self.buffer);
        let chars: Vec<char> = buf.chars().collect();
        let len = chars.len();
        let mut start = 0usize;
        let mut i = 0usize;

        while i < len {
            let ch = chars[i];

            // Double newline → paragraph break.
            if ch == '\n' && i + 1 < len && chars[i + 1] == '\n' {
                let segment: String = chars[start..i].iter().collect();
                let segment = segment.trim().to_string();
                if !segment.is_empty() {
                    sentences.push(segment);
                }
                i += 2;
                while i < len && chars[i] == '\n' {
                    i += 1;
                }
                start = i;
                continue;
            }

            // Single newline followed by an uppercase letter → implicit sentence end.
            if ch == '\n' && i + 1 < len && chars[i + 1].is_uppercase() {
                let segment: String = chars[start..i].iter().collect();
                let segment = segment.trim().to_string();
                if !segment.is_empty() {
                    sentences.push(segment);
                }
                i += 1;
                start = i;
                continue;
            }

            // Sentence-ending punctuation: '.', '!', '?'
            if matches!(ch, '.' | '!' | '?') {
                // The character after the punctuation must be whitespace, '"', ')', '\n',
                // or end-of-string — otherwise we are mid-token (e.g. a URL).
                let next_ok = if i + 1 >= len {
                    true
                } else {
                    matches!(chars[i + 1], ' ' | '\t' | '\n' | '"' | ')')
                };

                if next_ok {
                    // Find the start of the word immediately before this punctuation.
                    let mut word_start = i;
                    while word_start > start
                        && !chars[word_start - 1].is_whitespace()
                    {
                        word_start -= 1;
                    }
                    // The bare word (excluding the punctuation character itself).
                    let word: String = chars[word_start..i].iter().collect();
                    let word_lower = word.to_lowercase();
                    let is_single_upper =
                        word.len() == 1 && word.chars().all(|c| c.is_uppercase());
                    let is_abbrev = ABBREVS.contains(&word_lower.as_str());

                    if !is_single_upper && !is_abbrev {
                        let segment: String = chars[start..=i].iter().collect();
                        let segment = segment.trim().to_string();
                        if !segment.is_empty() {
                            sentences.push(segment);
                        }
                        // Skip the punctuation and any following whitespace.
                        i += 1;
                        while i < len && chars[i].is_whitespace() {
                            i += 1;
                        }
                        start = i;
                        continue;
                    }
                }
            }

            // Clause boundary: ',', ';', ':', or em-dash ' — ' / ' - '.
            // Split as soon as the current segment has MIN_WORDS_BEFORE_CLAUSE_SPLIT
            // words, so each chunk is a grammatically complete clause.
            let is_clause_break = matches!(ch, ',' | ';' | ':')
                || (ch == '-' && i > start && i + 1 < len
                    && chars[i - 1] == ' ' && chars[i + 1] == ' ');
            if is_clause_break {
                let segment_so_far: String = chars[start..i].iter().collect();
                let word_count = segment_so_far.split_whitespace().count();
                if word_count >= MIN_WORDS_BEFORE_CLAUSE_SPLIT {
                    let segment: String = chars[start..=i].iter().collect();
                    let segment = segment.trim().to_string();
                    if !segment.is_empty() {
                        sentences.push(segment);
                    }
                    i += 1;
                    if i < len && chars[i] == ' ' {
                        i += 1;
                    }
                    start = i;
                    continue;
                }
            }

            i += 1;
        }

        // Remaining text: flush or hold.
        let remaining: String = chars[start..].iter().collect();
        let remaining = remaining.trim().to_string();
        if flush_all {
            if !remaining.is_empty() {
                sentences.push(remaining);
            }
            self.buffer = String::new();
        } else {
            self.buffer = remaining;
        }

        sentences
    }
}
