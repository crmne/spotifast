import io

p = 'src/app.rs'
s = io.open(p, encoding='utf-8').read()

# 1. action arms: bump generation, stamp refresh
old = """            Action::JamStart => {
                // The host's player is whatever playback currently targets:
                // this computer's engine, Soloist, or another Connect device.
                let local_device_id = match self.target() {
                    crate::app::Target::Local => self.local_device_id.clone(),
                    crate::app::Target::Remote(id) => id.clone(),
                };
                self.jam_working = true;
                self.backend
                    .api(ApiRequest::JamStart { local_device_id });
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
                    self.backend.api(ApiRequest::JamLeave { session_id: jam.id });
                }
            }
            Action::JamEnd => {
                if let Some(jam) = self.jam.clone() {
                    self.jam_working = true;
                    self.backend.api(ApiRequest::JamEnd { session_id: jam.id });
                }
            }"""
new = """            Action::JamStart => {
                // The host's player is whatever playback currently targets:
                // this computer's engine, Soloist, or another Connect device.
                let local_device_id = match self.target() {
                    crate::app::Target::Local => self.local_device_id.clone(),
                    crate::app::Target::Remote(id) => id.clone(),
                };
                self.jam_generation += 1;
                self.jam_working = true;
                self.backend
                    .api(ApiRequest::JamStart { local_device_id });
            }
            Action::JamJoin(token) => {
                self.jam_generation += 1;
                self.jam_working = true;
                self.jam_invite_draft.clear();
                self.backend.api(ApiRequest::JamJoin { token });
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
                    self.backend.api(ApiRequest::JamLeave { session_id: jam.id });
                }
            }
            Action::JamEnd => {
                if let Some(jam) = self.jam.clone() {
                    self.jam_generation += 1;
                    self.jam_working = true;
                    self.backend.api(ApiRequest::JamEnd { session_id: jam.id });
                }
            }"""
assert old in s, 'action arms not found'
s = s.replace(old, new)

# 2. popup-open sends stamped
old = """                    // A Jam's member list moves while people join; the
                    // picker asks once at open and again on its timer.
                    self.backend.api(ApiRequest::JamRefresh);"""
new = """                    // A Jam's member list moves while people join; the
                    // picker asks once at open and again on its timer.
                    self.backend.api(ApiRequest::JamRefresh {
                        generation: self.jam_generation,
                    });"""
assert old in s, 'popup open send not found'
s = s.replace(old, new)

old = """                self.show_devices = true;
                self.refresh_devices();
                self.backend.api(ApiRequest::JamRefresh);"""
new = """                self.show_devices = true;
                self.refresh_devices();
                self.backend.api(ApiRequest::JamRefresh {
                    generation: self.jam_generation,
                });"""
assert old in s, 'transfer-open send not found'
s = s.replace(old, new)

# 3. JamState arm: generation guard + reindent Ok arms
old = """            ApiResponse::JamState { result } => match result {
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
            },
            ApiResponse::JamGone"""
new = """            ApiResponse::JamState { generation, result } => {
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
            }
            ApiResponse::JamGone"""
assert old in s, 'JamState arm not found'
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('app generation wiring done')
