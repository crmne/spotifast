//! Read-only song-radio resolution through the existing librespot session.

use anyhow::{Context as _, Result, ensure};
use librespot_core::{Session, SpotifyId, SpotifyUri};
use librespot_metadata::{Metadata, Track as MetadataTrack};
use librespot_protocol::context::Context;

use crate::api::models::{Album, ArtistRef, ExternalIds, Image, Track};
use crate::model::Loadable;

#[derive(Clone, Debug, Default)]
pub struct Station {
    pub seed: Track,
    pub tracks: Vec<Track>,
}

#[derive(Default)]
pub struct RadioPage {
    /// Keep the header independent of recommendation requests and cache eviction.
    pub seed: Option<Track>,
    pub station: Loadable<Station>,
    pub generation: u64,
}

/// Resolving a context and reading metadata never activates Connect or loads audio.
pub async fn resolve(session: &Session, seed: &str) -> Result<Station> {
    let seed_uri = SpotifyUri::from_uri(&format!("spotify:track:{seed}"))?;
    let context = session
        .spclient()
        .get_context(&format!("spotify:station:track:{seed}"))
        .await
        .context("Couldn't load song radio")?;
    let uris = station_uris(&context)?;
    ensure!(
        !uris.is_empty(),
        "Spotify returned no songs for this radio. Try again."
    );
    let image_url = session
        .get_user_attribute("image-url")
        .unwrap_or_else(|| "https://i.scdn.co/image/{file_id}".into());
    let seed = metadata_track(MetadataTrack::get(session, &seed_uri).await?, &image_url)?;
    let mut tracks = Vec::with_capacity(uris.len());
    // Bound metadata concurrency, and retain Spotify's order despite responses
    // arriving out of order. These requests do not consume the shared Web API quota.
    for chunk in uris.chunks(4) {
        let mut tasks = tokio::task::JoinSet::new();
        for (index, uri) in chunk.iter().cloned().enumerate() {
            let session = session.clone();
            tasks.spawn(async move { (index, MetadataTrack::get(&session, &uri).await) });
        }
        let mut batch = Vec::with_capacity(chunk.len());
        while let Some(result) = tasks.join_next().await {
            let (index, track) = result?;
            batch.push((index, metadata_track(track?, &image_url)?));
        }
        batch.sort_by_key(|(index, _)| *index);
        tracks.extend(batch.into_iter().map(|(_, track)| track));
    }
    Ok(Station { seed, tracks })
}

fn station_uris(context: &Context) -> Result<Vec<SpotifyUri>> {
    context
        .pages
        .iter()
        .flat_map(|page| &page.tracks)
        .map(|track| {
            let uri = match track.uri.as_deref().filter(|uri| !uri.is_empty()) {
                Some(uri) => SpotifyUri::from_uri(uri)?,
                None => SpotifyUri::Track {
                    id: SpotifyId::from_raw(
                        track
                            .gid
                            .as_deref()
                            .context("Spotify returned a radio song without an identifier")?,
                    )?,
                },
            };
            ensure!(
                matches!(uri, SpotifyUri::Track { .. }),
                "Spotify returned a non-song radio item"
            );
            Ok(uri)
        })
        .collect()
}

fn metadata_track(track: MetadataTrack, image_url: &str) -> Result<Track> {
    let artists = track
        .artists
        .iter()
        .map(|artist| {
            Ok(ArtistRef {
                id: Some(artist.id.to_id()?),
                uri: Some(artist.id.to_uri()?),
                name: artist.name.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let images = track
        .album
        .covers
        .iter()
        .map(|image| Image {
            url: image_url.replace("{file_id}", &image.id.to_string()),
            width: u32::try_from(image.width).ok(),
            height: u32::try_from(image.height).ok(),
        })
        .collect();
    Ok(Track {
        id: Some(track.id.to_id()?),
        uri: track.id.to_uri()?,
        name: track.name,
        duration_ms: u32::try_from(track.duration).unwrap_or_default(),
        explicit: track.is_explicit,
        external_ids: ExternalIds {
            isrc: track
                .external_ids
                .iter()
                .find(|id| id.external_type.eq_ignore_ascii_case("isrc"))
                .map(|id| id.id.clone()),
        },
        artists,
        album: Some(Album {
            id: track.album.id.to_id()?,
            uri: track.album.id.to_uri()?,
            name: track.album.name,
            images,
            ..Album::default()
        }),
        track_number: u32::try_from(track.number).ok(),
        disc_number: u32::try_from(track.disc_number).ok(),
        popularity: u8::try_from(track.popularity).ok(),
        ..Track::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_protocol::{context_page::ContextPage, context_track::ContextTrack};

    #[test]
    fn radio_metadata_preserves_recording_identity() {
        use librespot_protocol::metadata;
        let message = metadata::Track {
            gid: Some(vec![1; 16]),
            album: Some(metadata::Album {
                gid: Some(vec![2; 16]),
                date: Some(metadata::Date {
                    year: Some(2020),
                    ..Default::default()
                })
                .into(),
                ..Default::default()
            })
            .into(),
            external_id: vec![metadata::ExternalId {
                type_: Some("isrc".into()),
                id: Some("GBUM71029604".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let track =
            metadata_track(MetadataTrack::try_from(&message).unwrap(), "{file_id}").unwrap();
        assert_eq!(track.recording_key().as_deref(), Some("isrc:GBUM71029604"));
    }

    #[test]
    fn radio_identifiers_preserve_order_and_accept_binary_ids() {
        let context = Context {
            pages: vec![ContextPage {
                tracks: vec![
                    ContextTrack {
                        uri: Some("spotify:track:3JA9Jsuxr4xgHXEawAdCp4".into()),
                        ..Default::default()
                    },
                    ContextTrack {
                        gid: Some(vec![0; 16]),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let uris = station_uris(&context)
            .unwrap()
            .iter()
            .map(|uri| uri.to_uri().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            uris,
            [
                "spotify:track:3JA9Jsuxr4xgHXEawAdCp4",
                "spotify:track:0000000000000000000000"
            ]
        );
    }

    #[test]
    fn malformed_radio_items_are_errors_instead_of_silent_omissions() {
        for track in [
            ContextTrack::default(),
            ContextTrack {
                gid: Some(vec![0; 5]),
                ..Default::default()
            },
            ContextTrack {
                uri: Some("spotify:album:3JA9Jsuxr4xgHXEawAdCp4".into()),
                ..Default::default()
            },
        ] {
            let context = Context {
                pages: vec![ContextPage {
                    tracks: vec![track],
                    ..Default::default()
                }],
                ..Default::default()
            };
            assert!(station_uris(&context).is_err());
        }
    }
}
