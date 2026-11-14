import io

p = 'src/app.rs'
s = io.open(p, encoding='utf-8').read()
n0 = s

def rep(old, new, tag):
    global s
    if old not in s:
        fails.append(tag)
        return
    s = s.replace(old, new)

fails = []

# 1. state field + doc
rep("""    pub jam_detail: std::collections::HashMap<String, crate::jam::JamMember>,
    pub jam_member_images: std::collections::HashMap<String, String>,""",
"""    pub jam_detail: std::collections::HashMap<String, crate::jam::JamMember>,
    pub jam_member_images: std::collections::HashMap<String, String>,
    /// Rises with every Jam action, so answers that left before the
    /// newest action cannot overwrite its result.
    jam_generation: u64,""", 'A1 field')

# 2. constructor
rep("""            jam_detail: std::collections::HashMap::new(),
            jam_member_images: std::collections::HashMap::new(),""",
"""            jam_detail: std::collections::HashMap::new(),
            jam_member_images: std::collections::HashMap::new(),
            jam_generation: 0,""", 'A2 init')

# 3. reset_data bump (jam block ends with member_images clear)
rep("""        self.jam_invite_draft.clear();
        self.jam_polled_at = None;
        self.jam_detail.clear();
        self.jam_member_images.clear();""",
"""        self.jam_invite_draft.clear();
        self.jam_polled_at = None;
        self.jam_detail.clear();
        self.jam_member_images.clear();
        self.jam_generation += 1;""", 'A3 reset')

# 4. action arms
rep("""            Action::JamStart => {
                // The host's player is whatever playback currently targets:
                // this computer's engine, Soloist, or another Connect device.
                let local_device_id = match self.target() {
                    crate::app::Target::Local => self.local_device_id.clone(),
                    crate::app::Target::Remote(id) => id.clone(),
                };
                self.jam_working = true;
                self.backend.api(ApiRequest::JamStart { local_device_id });
            }
            Action::JamJoin(token) => {
                self.jam_working = true;
                self.jam_invite_draft.clear();
                self.backend.api(ApiRequest::JamJoin { token });
            }
            Action::JamRefresh => {
                self.backend.api(ApiRequest::JamRefresh);
            }
            Action::JamLeave => {
                if let Some(jam) = self.jam.clone() {
                    self.jam_working = true;
                    self.backend
                        .api(ApiRequest::JamLeave { session_id: jam.id });
                }
            }
            Action::JamEnd => {
                if let Some(jam) = self.jam.clone() {
                    self.jam_working = true;
                    self.backend.api(ApiRequest::JamEnd { session_id: jam.id });
                }
            }""",
"""            Action::JamStart => {
                // The host's player is whatever playback currently targets:
                // this computer's engine, Soloist, or another Connect device.
                let local_device_id = match self.target() {
                    crate::app::Target::Local => self.local_device_id.clone(),
                    crate::app::Target::Remote(id) => id.clone(),
                };
                self.jam_generation += 1;
                self.jam_working = true;
                self.backend.api(ApiRequest::JamStart {
                    local_device_id,
                    generation: self.jam_generation,
                });
            }
            Action::JamJoin(token) => {
                self.jam_generation += 1;
                self.jam_working = true;
                self.jam_invite_draft.clear();
                self.backend.api(ApiRequest::JamJoin {
                    token,
                    generation: self.jam_generation,
                });
            }
            Action::JamRefresh => {
                self.backend.api(ApiRequest::JamRefresh {
                    generation: self.jam_generation,
                });
            }
            Action::JamLeave => {
                if let Some(jam) = self.jam.clone() {
                    self.jam_generation += 1;
                    self.jam_working = true;
                    self.backend.api(ApiRequest::JamLeave {
                        session_id: jam.id,
                        generation: self.jam_generation,
                    });
                }
            }
            Action::JamEnd => {
                if let Some(jam) = self.jam.clone() {
                    self.jam_generation += 1;
                    self.jam_working = true;
                    self.backend.api(ApiRequest::JamEnd {
                        session_id: jam.id,
                        generation: self.jam_generation,
                    });
                }
            }""", 'A4 actions')

# 5. popup-open stamped
rep("""                    // A Jam's member list moves while people join; the
                    // picker asks once at open and again on its timer.
                    self.backend.api(ApiRequest::JamRefresh);""",
"""                    // A Jam's member list moves while people join; the
                    // picker asks once at open and again on its timer.
                    self.backend.api(ApiRequest::JamRefresh {
                        generation: self.jam_generation,
                    });""", 'A5 popup')

# 6. transfer-open stamped + gate on local_ready (P2: no error when playback down)
rep("""                self.show_devices = true;
                self.refresh_devices();
                self.backend.api(ApiRequest::JamRefresh);""",
"""                self.show_devices = true;
                self.refresh_devices();
                if self.local_ready {
                    self.backend.api(ApiRequest::JamRefresh {
                        generation: self.jam_generation,
                    });
                }""", 'A6 transfer')

# 7. ToggleDevicesPopup: gate on local_ready (find its send)
rep("""                    // A Jam's member list moves while people join; the
                    // picker asks once at open and again on its timer.
                    self.backend.api(ApiRequest::JamRefresh {
                        generation: self.jam_generation,
                    });""",
"""                    // A Jam's member list moves while people join; the
                    // picker asks once at open and again on its timer,
                    // and only when the session a Jam travels over exists.
                    if self.local_ready {
                        self.backend.api(ApiRequest::JamRefresh {
                            generation: self.jam_generation,
                        });
                    }""", 'A7 popup gate')

# 8. JamState arm with stale guard
rep("""            ApiResponse::JamState { result } => match result {
                Ok(Some(session)) => {
                    let fresh = self
                        .jam
                        .as_ref()
                        .is_none_or(|current| current.id != session.id);
                    self.jam = Some(session.clone());
                    self.jam_working = false;
                    self.jam_polled_at = Some(Instant::now());
                    // Joining changes nothing about where music plays: the
                    // session decides that, and the picker shows the same
                    // devices as before. A new session asks for a device
                    // list refresh and everyone's portraits.
                    if fresh {
                        self.refresh_devices();
                        self.request_jam_detail();
                    }
                }
                Ok(None) => {
                    self.jam = None;
                    self.jam_working = false;
                    self.jam_polled_at = Some(Instant::now());
                    self.jam_detail.clear();
                    self.jam_member_images.clear();
                }
                Err(error) => {
                    self.jam_working = false;
                    self.jam_polled_at = Some(Instant::now());
                    self.toast_error(
                        // Translators: {error} is an error message.
                        gettext(self.locale, "Couldn't open the Jam: {error}")
                            .replace("{error}", &error.to_string()),
                    );
                }
            },""",
"""            ApiResponse::JamState { generation, result } => {
                // An answer that left before the newest action carries an
                // older generation: applying it would undo a join, a
                // departure or a fresh start that has already happened.
                if generation < self.jam_generation {
                    return;
                }
                match result {
                    Ok(Some(session)) => {
                        let fresh = self
                            .jam
                            .as_ref()
                            .is_none_or(|current| current.id != session.id);
                        self.jam = Some(session.clone());
                        self.jam_working = false;
                        self.jam_polled_at = Some(Instant::now());
                        // Joining changes nothing about where music plays:
                        // the session decides that, and the picker shows
                        // the same devices as before. A new session asks
                        // for a device list refresh and everyone's
                        // portraits.
                        if fresh {
                            self.refresh_devices();
                            self.request_jam_detail();
                        }
                    }
                    Ok(None) => {
                        self.jam = None;
                        self.jam_working = false;
                        self.jam_polled_at = Some(Instant::now());
                        self.jam_detail.clear();
                        self.jam_member_images.clear();
                    }
                    Err(error) => {
                        self.jam_working = false;
                        self.jam_polled_at = Some(Instant::now());
                        self.toast_error(
                            // Translators: {error} is an error message.
                            gettext(self.locale, "Couldn't open the Jam: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                    }
                }
            }""", 'A8 stale guard')

# 9. JamGone keep-on-error
rep("""            ApiResponse::JamGone { result } => {
                self.jam = None;
                self.jam_working = false;
                self.jam_detail.clear();
                self.jam_member_images.clear();
                if let Err(error) = result {
                    self.toast_error(
                        // Translators: {error} is an error message.
                        gettext(self.locale, "Couldn't leave the Jam: {error}")
                            .replace("{error}", &error.to_string()),
                    );
                } else {
                    self.refresh_devices();
                }
            }""",
"""            ApiResponse::JamGone { generation, result } => {
                self.jam_working = false;
                // A stale departure answer cannot clear a jam this
                // account has already rejoined.
                if generation < self.jam_generation {
                    return;
                }
                if let Err(error) = result {
                    // The departure never landed: keep the session and
                    // its portraits so the picker keeps the retry
                    // controls and the poll keeps running.
                    self.toast_error(
                        // Translators: {error} is an error message.
                        gettext(self.locale, "Couldn't leave the Jam: {error}")
                            .replace("{error}", &error.to_string()),
                    );
                } else {
                    self.jam = None;
                    self.jam_detail.clear();
                    self.jam_member_images.clear();
                    self.refresh_devices();
                }
            }""", 'A9 gone')

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('FAILURES:', fails or 'none')
