import io, sys

fails = []
def patch(path, old, new, tag):
    s = io.open(path, encoding='utf-8').read()
    if old not in s:
        fails.append(tag)
        return
    io.open(path, 'w', encoding='utf-8', newline='\n').write(s.replace(old, new))

B = 'src/backend.rs'
A = 'src/app.rs'
J = 'src/ui/jam.rs'
W = 'src/ui/widgets.rs'

# ---- backend: request + response generation ----
patch(B, """    /// Asks which Jam this account is in, if any.
    JamRefresh,""", """    /// Asks which Jam this account is in, if any. The generation lets
    /// the interface discard answers that left before a newer join,
    /// leave or end.
    JamRefresh { generation: u64 },""", 'B1 request')

patch(B, """    /// The Jam this account is in after a start, a join, or a refresh;
    /// `None` when there is none.
    JamState {
        result: ApiResult<Option<crate::jam::JamSession>>,
    },""", """    /// The Jam this account is in after a start, a join, or a refresh;
    /// `None` when there is none. The generation is the request's, so a
    /// stale answer cannot overwrite a newer departure.
    JamState {
        generation: u64,
        result: ApiResult<Option<crate::jam::JamSession>>,
    },""", 'B2 response')

patch(B, """    if let ApiRequest::JamRefresh = request {
        let result = match jam_session(api, engine) {
            Ok(session) => crate::jam::current(session)
                .await
                .map_err(|error| ApiError::Network(format!("{error:#}"))),
            Err(error) => Err(error),
        };
        return (ApiResponse::JamState { result }, Vec::new());
    }""", """    if let ApiRequest::JamRefresh { generation } = request {
        let result = match jam_session(api, engine) {
            Ok(session) => crate::jam::current(session)
                .await
                .map_err(|error| ApiError::Network(format!("{error:#}"))),
            Err(error) => Err(error),
        };
        return (
            ApiResponse::JamState {
                generation,
                result,
            },
            Vec::new(),
        );
    }""", 'B3 worker')

patch(B, """        | ApiRequest::JamRefresh
        | ApiRequest::JamDetail { .. } => Operation::Playback,""", """        | ApiRequest::JamRefresh { .. }
        | ApiRequest::JamDetail { .. } => Operation::Playback,""", 'B4 op')

patch(B, """        | ApiRequest::JamRefresh
        | ApiRequest::JamDetail { .. } => unreachable!("handled before Web API routing"),""", """        | ApiRequest::JamRefresh { .. }
        | ApiRequest::JamDetail { .. } => unreachable!("handled before Web API routing"),""", 'B5 unreachable')

if fails:
    print('FAILED PATCHES:', fails)
    sys.exit(1)
print('backend patched cleanly')
