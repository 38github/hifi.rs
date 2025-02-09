use crate::{
    client::{
        album::{Album, AlbumSearchResults},
        artist::{Artist, ArtistSearchResults},
        playlist::{Playlist, UserPlaylistsResult},
        search_results::SearchAllResults,
        track::Track,
        AudioQuality, TrackURL,
    },
    Error, Result,
};
use base64::{engine::general_purpose, Engine as _};
use clap::ValueEnum;
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Method, Response, StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

const BUNDLE_REGEX: &str =
    r#"<script src="(/resources/\d+\.\d+\.\d+-[a-z0-9]\d{3}/bundle\.js)"></script>"#;
const APP_REGEX: &str =
    r#"production:\{api:\{appId:"(?P<app_id>\d{9})",appSecret:"(?P<app_secret>\w{32})""#;
const SEED_REGEX: &str =
    r#"[a-z]\.initialSeed\("(?P<seed>[\w=]+)",window\.utimezone\.(?P<timezone>[a-z]+)\)"#;

macro_rules! info_regex {
    () => {
        r#"name:"\w+/(?P<timezone>{}([a-z]?))",info:"(?P<info>[\w=]+)",extras:"(?P<extras>[\w=]+)""#
    };
}

#[derive(Debug, Clone)]
pub struct Client {
    secrets: HashMap<String, String>,
    active_secret: Option<String>,
    app_id: Option<String>,
    base_url: String,
    client: reqwest::Client,
    default_quality: AudioQuality,
    user_token: Option<String>,
    bundle_regex: regex::Regex,
    app_id_regex: regex::Regex,
    seed_regex: regex::Regex,
}

pub async fn new(
    active_secret: Option<String>,
    app_id: Option<String>,
    audio_quality: Option<AudioQuality>,
    user_token: Option<String>,
) -> Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
            "User-Agent",
            HeaderValue::from_str(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/111.0.0.0 Safari/537.36",
            )
            .unwrap(),
        );

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .default_headers(headers)
        .build()
        .unwrap();

    let default_quality = if let Some(quality) = audio_quality {
        quality
    } else {
        AudioQuality::Mp3
    };

    Ok(Client {
        client,
        secrets: HashMap::new(),
        active_secret,
        user_token,
        app_id,
        default_quality,
        base_url: "https://www.qobuz.com/api.json/0.2/".to_string(),
        bundle_regex: regex::Regex::new(BUNDLE_REGEX).unwrap(),
        app_id_regex: regex::Regex::new(APP_REGEX).unwrap(),
        seed_regex: regex::Regex::new(SEED_REGEX).unwrap(),
    })
}

#[non_exhaustive]
enum Endpoint {
    Album,
    Artist,
    Login,
    Track,
    UserPlaylist,
    SearchArtists,
    SearchAlbums,
    TrackURL,
    Playlist,
    PlaylistCreate,
    PlaylistDelete,
    PlaylistAddTracks,
    PlaylistDeleteTracks,
    PlaylistUpdatePosition,
    Search,
}

impl Endpoint {
    fn as_str(&self) -> &str {
        match self {
            Endpoint::Album => "album/get",
            Endpoint::Artist => "artist/get",
            Endpoint::Login => "user/login",
            Endpoint::Playlist => "playlist/get",
            Endpoint::PlaylistCreate => "playlist/create",
            Endpoint::PlaylistDelete => "playlist/delete",
            Endpoint::PlaylistAddTracks => "playlist/addTracks",
            Endpoint::PlaylistDeleteTracks => "playlist/deleteTracks",
            Endpoint::PlaylistUpdatePosition => "playlist/updateTracksPosition",
            Endpoint::Search => "catalog/search",
            Endpoint::SearchAlbums => "album/search",
            Endpoint::SearchArtists => "artist/search",
            Endpoint::Track => "track/get",
            Endpoint::TrackURL => "track/getFileUrl",
            Endpoint::UserPlaylist => "playlist/getUserPlaylists",
        }
    }
}

macro_rules! get {
    ($self:ident, $endpoint:expr, $params:expr) => {
        match $self.make_get_call($endpoint, $params).await {
            Ok(response) => match serde_json::from_str(response.as_str()) {
                Ok(item) => Ok(item),
                Err(error) => Err(Error::DeserializeJSON {
                    message: error.to_string(),
                }),
            },
            Err(error) => Err(Error::Api {
                message: error.to_string(),
            }),
        }
    };
}

macro_rules! post {
    ($self:ident, $endpoint:expr, $form:expr) => {
        match $self.make_post_call($endpoint, $form).await {
            Ok(response) => match serde_json::from_str(response.as_str()) {
                Ok(item) => Ok(item),
                Err(error) => Err(Error::DeserializeJSON {
                    message: error.to_string(),
                }),
            },
            Err(error) => Err(Error::Api {
                message: error.to_string(),
            }),
        }
    };
}

impl Client {
    pub fn quality(&self) -> AudioQuality {
        self.default_quality.clone()
    }

    pub fn signed_in(&self) -> bool {
        self.user_token.is_some()
    }

    /// Login a user
    pub async fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::Login.as_str());

        if let Some(app_id) = &self.app_id {
            info!(
                "logging in with email ({}) and password **HIDDEN** for app_id {}",
                username, app_id
            );

            let params = vec![
                ("email", username),
                ("password", password),
                ("app_id", app_id.as_str()),
            ];

            match self.make_get_call(endpoint, Some(params)).await {
                Ok(response) => {
                    let json: Value = serde_json::from_str(response.as_str()).unwrap();
                    info!("Successfully logged in");
                    debug!("{}", json);
                    let mut token = json["user_auth_token"].to_string();
                    token = token[1..token.len() - 1].to_string();

                    self.user_token = Some(token);
                    Ok(())
                }
                Err(err) => {
                    error!("error logging into qobuz: {}", err);
                    Err(Error::Login)
                }
            }
        } else {
            Err(Error::Login)
        }
    }

    /// Retrieve a list of the user's playlists
    pub async fn user_playlists(&self) -> Result<UserPlaylistsResult> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::UserPlaylist.as_str());
        let params = vec![("limit", "500"), ("extra", "tracks"), ("offset", "0")];

        get!(self, endpoint, Some(params))
    }

    /// Retrieve a playlist
    pub async fn playlist(&self, playlist_id: i64) -> Result<Playlist> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::Playlist.as_str());
        let id_string = playlist_id.to_string();
        let params = vec![
            ("limit", "500"),
            ("extra", "tracks"),
            ("playlist_id", id_string.as_str()),
            ("offset", "0"),
        ];
        let playlist: Result<Playlist> = get!(self, endpoint.clone(), Some(params.clone()));

        if let Ok(mut playlist) = playlist {
            if let Ok(all_items_playlist) = self.playlist_items(&mut playlist, endpoint).await {
                Ok(all_items_playlist.clone())
            } else {
                Err(Error::Api {
                    message: "error fetching playlist".to_string(),
                })
            }
        } else {
            Err(Error::Api {
                message: "error fetching playlist".to_string(),
            })
        }
    }

    async fn playlist_items<'p>(
        &self,
        playlist: &'p mut Playlist,
        endpoint: String,
    ) -> Result<&'p Playlist> {
        let total_tracks = playlist.tracks_count as usize;
        let mut all_tracks: Vec<Track> = Vec::new();

        if let Some(mut tracks) = playlist.tracks.clone() {
            all_tracks.append(&mut tracks.items);

            while all_tracks.len() < total_tracks {
                let id = playlist.id.to_string();
                let limit_string = (total_tracks - all_tracks.len()).to_string();
                let offset_string = all_tracks.len().to_string();

                let params = vec![
                    ("limit", limit_string.as_str()),
                    ("extra", "tracks"),
                    ("playlist_id", id.as_str()),
                    ("offset", offset_string.as_str()),
                ];

                let playlist: Result<Playlist> = get!(self, endpoint.clone(), Some(params));

                match &playlist {
                    Ok(playlist) => {
                        debug!("appending tracks to playlist");
                        if let Some(new_tracks) = &playlist.tracks {
                            all_tracks.append(&mut new_tracks.clone().items);
                        }
                    }
                    Err(error) => error!("{}", error.to_string()),
                }
            }

            if !all_tracks.is_empty() {
                tracks.items = all_tracks;
                playlist.set_tracks(tracks);
            }
        }

        Ok(playlist)
    }

    pub async fn create_playlist(
        &self,
        name: String,
        is_public: bool,
        description: Option<String>,
        is_collaborative: Option<bool>,
    ) -> Result<Playlist> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::PlaylistCreate.as_str());

        let mut form_data = HashMap::new();
        form_data.insert("name", name.as_str());

        let is_collaborative = if !is_public || is_collaborative.is_none() {
            "false".to_string()
        } else if let Some(is_collaborative) = is_collaborative {
            is_collaborative.to_string()
        } else {
            "false".to_string()
        };

        form_data.insert("is_collaborative", is_collaborative.as_str());

        let is_public = is_public.to_string();
        form_data.insert("is_public", is_public.as_str());

        let description = if let Some(description) = description {
            description
        } else {
            "".to_string()
        };
        form_data.insert("description", description.as_str());

        post!(self, endpoint, form_data)
    }

    pub async fn delete_playlist(&self, playlist_id: String) -> Result<SuccessfulResponse> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::PlaylistDelete.as_str());

        let mut form_data = HashMap::new();
        form_data.insert("playlist_id", playlist_id.as_str());

        post!(self, endpoint, form_data)
    }

    /// Add new track to playlist
    pub async fn playlist_add_track(
        &self,
        playlist_id: String,
        track_ids: Vec<String>,
    ) -> Result<Playlist> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::PlaylistAddTracks.as_str());

        let track_ids = track_ids.join(",");

        let mut form_data = HashMap::new();
        form_data.insert("playlist_id", playlist_id.as_str());
        form_data.insert("track_ids", track_ids.as_str());
        form_data.insert("no_duplicate", "true");

        post!(self, endpoint, form_data)
    }

    /// Add new track to playlist
    pub async fn playlist_delete_track(
        &self,
        playlist_id: String,
        playlist_track_ids: Vec<String>,
    ) -> Result<Playlist> {
        let endpoint = format!(
            "{}{}",
            self.base_url,
            Endpoint::PlaylistDeleteTracks.as_str()
        );

        let playlist_track_ids = playlist_track_ids.join(",");

        let mut form_data = HashMap::new();
        form_data.insert("playlist_id", playlist_id.as_str());
        form_data.insert("playlist_track_ids", playlist_track_ids.as_str());

        post!(self, endpoint, form_data)
    }

    /// Update track position in playlist
    pub async fn playlist_track_position(
        &self,
        index: usize,
        playlist_id: String,
        track_id: String,
    ) -> Result<Playlist> {
        let endpoint = format!(
            "{}{}",
            self.base_url,
            Endpoint::PlaylistUpdatePosition.as_str()
        );

        let index = index.to_string();

        let mut form_data = HashMap::new();
        form_data.insert("playlist_id", playlist_id.as_str());
        form_data.insert("playlist_track_ids", track_id.as_str());
        form_data.insert("insert_before", index.as_str());

        post!(self, endpoint, form_data)
    }

    /// Retrieve track information
    pub async fn track(&self, track_id: i32) -> Result<Track> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::Track.as_str());
        let track_id_string = track_id.to_string();
        let params = vec![("track_id", track_id_string.as_str())];

        get!(self, endpoint, Some(params))
    }

    /// Retrieve url information for a track's audio file
    pub async fn track_url(
        &self,
        track_id: i32,
        fmt_id: Option<AudioQuality>,
        sec: Option<String>,
    ) -> Result<TrackURL> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::TrackURL.as_str());
        let now = format!("{}", chrono::Utc::now().timestamp());
        let secret = if let Some(secret) = sec {
            secret
        } else if let Some(s) = &self.active_secret {
            s.clone()
        } else {
            return Err(Error::ActiveSecret);
        };

        let format_id = if let Some(quality) = fmt_id {
            quality
        } else {
            self.quality()
        };

        let sig = format!(
            "trackgetFileUrlformat_id{}intentstreamtrack_id{}{}{}",
            format_id.clone(),
            track_id,
            now,
            secret
        );
        let hashed_sig = format!("{:x}", md5::compute(sig.as_str()));

        let track_id = track_id.to_string();
        let format_string = format_id.to_string();

        let params = vec![
            ("request_ts", now.as_str()),
            ("request_sig", hashed_sig.as_str()),
            ("track_id", track_id.as_str()),
            ("format_id", format_string.as_str()),
            ("intent", "stream"),
        ];

        get!(self, endpoint, Some(params))
    }

    pub async fn search_all(&self, query: String, limit: i32) -> Result<SearchAllResults> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::Search.as_str());
        let limit = limit.to_string();
        let params = vec![("query", query.as_str()), ("limit", &limit)];

        get!(self, endpoint, Some(params))
    }

    // Retrieve information about an album
    pub async fn album(&self, album_id: &str) -> Result<Album> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::Album.as_str());
        let params = vec![("album_id", album_id)];

        get!(self, endpoint, Some(params))
    }

    // Search the database for albums
    pub async fn search_albums(
        &self,
        query: String,
        limit: Option<i32>,
    ) -> Result<AlbumSearchResults> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::SearchAlbums.as_str());
        let limit = if let Some(limit) = limit {
            limit.to_string()
        } else {
            100.to_string()
        };
        let params = vec![("query", query.as_str()), ("limit", limit.as_str())];

        get!(self, endpoint, Some(params))
    }

    // Retrieve information about an artist
    pub async fn artist(&self, artist_id: i32, limit: Option<i32>) -> Result<Artist> {
        if let Some(app_id) = &self.app_id {
            let endpoint = format!("{}{}", self.base_url, Endpoint::Artist.as_str());
            let limit = if let Some(limit) = limit {
                limit.to_string()
            } else {
                100.to_string()
            };

            let artistid_string = artist_id.to_string();

            let params = vec![
                ("artist_id", artistid_string.as_str()),
                ("app_id", app_id),
                ("limit", limit.as_str()),
                ("offset", "0"),
                ("extra", "albums"),
            ];

            get!(self, endpoint, Some(params))
        } else {
            Err(Error::AppID)
        }
    }

    // Search the database for artists
    pub async fn search_artists(
        &self,
        query: String,
        limit: Option<i32>,
    ) -> Result<ArtistSearchResults> {
        let endpoint = format!("{}{}", self.base_url, Endpoint::SearchArtists.as_str());
        let limit = if let Some(limit) = limit {
            limit.to_string()
        } else {
            100.to_string()
        };
        let params = vec![("query", query.as_str()), ("limit", &limit)];

        get!(self, endpoint, Some(params))
    }

    // Set a user access token for authentication
    pub fn set_token(&mut self, token: String) {
        self.user_token = Some(token);
    }

    // Set an app_id for authentication
    pub fn set_app_id(&mut self, app_id: String) {
        self.app_id = Some(app_id);
    }

    // Set an app secret for authentication
    pub fn set_active_secret(&mut self, active_secret: String) {
        self.active_secret = Some(active_secret);
    }

    pub fn set_default_quality(&mut self, quality: AudioQuality) {
        self.default_quality = quality;
    }

    pub fn get_token(&self) -> Option<String> {
        self.user_token.clone()
    }

    pub fn get_active_secret(&self) -> Option<String> {
        self.active_secret.clone()
    }

    pub fn get_app_id(&self) -> Option<String> {
        self.app_id.clone()
    }

    fn client_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();

        if let Some(app_id) = &self.app_id {
            info!("adding app_id to request headers: {}", app_id);
            headers.insert("X-App-Id", HeaderValue::from_str(app_id).unwrap());
        } else {
            error!("no app_id");
        }

        if let Some(token) = &self.user_token {
            info!("adding token to request headers: {}", token);
            headers.insert(
                "X-User-Auth-Token",
                HeaderValue::from_str(token.as_str()).unwrap(),
            );
        }

        headers
    }

    // Make a GET call to the API with the provided parameters
    async fn make_get_call(
        &self,
        endpoint: String,
        params: Option<Vec<(&str, &str)>>,
    ) -> Result<String> {
        let headers = self.client_headers();

        debug!("calling {} endpoint, with params {params:?}", endpoint);
        let request = self.client.request(Method::GET, endpoint).headers(headers);

        if let Some(p) = params {
            let response = request.query(&p).send().await?;
            self.handle_response(response).await
        } else {
            let response = request.send().await?;
            self.handle_response(response).await
        }
    }

    // Make a POST call to the API with form data
    async fn make_post_call(
        &self,
        endpoint: String,
        params: HashMap<&str, &str>,
    ) -> Result<String> {
        let headers = self.client_headers();

        debug!("calling {} endpoint, with params {params:?}", endpoint);
        let response = self
            .client
            .request(Method::POST, endpoint)
            .headers(headers)
            .form(&params)
            .send()
            .await?;

        self.handle_response(response).await
    }

    // Handle a response retrieved from the api
    async fn handle_response(&self, response: Response) -> Result<String> {
        if response.status() == StatusCode::OK {
            let res = response.text().await.unwrap();
            Ok(res)
        } else {
            Err(Error::Api {
                message: response.status().to_string(),
            })
        }
    }

    // ported from https://github.com/vitiko98/qobuz-dl/blob/master/qobuz_dl/bundle.py
    // Retrieve the app_id and generate the secrets needed to authenticate
    pub async fn refresh(&mut self) -> Result<()> {
        debug!("fetching login page");
        let play_url = "https://play.qobuz.com";
        let login_page = self.client.get(format!("{play_url}/login")).send().await?;

        let contents = login_page.text().await.unwrap();

        if let Some(captures) = self.bundle_regex.captures(contents.as_str()) {
            let bundle_path = captures.get(1).map_or("", |m| m.as_str());
            let bundle_url = format!("{play_url}{bundle_path}");
            if let Ok(bundle_page) = self.client.get(bundle_url).send().await {
                if let Ok(bundle_contents) = bundle_page.text().await {
                    if let Some(captures) = self.app_id_regex.captures(bundle_contents.as_str()) {
                        let app_id = captures
                            .name("app_id")
                            .map_or("".to_string(), |m| m.as_str().to_string());

                        self.app_id = Some(app_id.clone());

                        let seed_data = self.seed_regex.captures_iter(bundle_contents.as_str());

                        seed_data.for_each(|s| {
                            let seed = s.name("seed").map_or("", |m| m.as_str()).to_string();
                            let mut timezone =
                                s.name("timezone").map_or("", |m| m.as_str()).to_string();
                            crate::client::capitalize(timezone.as_mut_str());

                            let info_regex = format!(info_regex!(), &timezone);
                            regex::Regex::new(info_regex.as_str())
                                .unwrap()
                                .captures_iter(bundle_contents.as_str())
                                .for_each(|c| {
                                    let timezone =
                                        c.name("timezone").map_or("", |m| m.as_str()).to_string();
                                    let info =
                                        c.name("info").map_or("", |m| m.as_str()).to_string();
                                    let extras =
                                        c.name("extras").map_or("", |m| m.as_str()).to_string();

                                    let chars = format!("{seed}{info}{extras}");

                                    let encoded_secret = chars[..chars.len() - 44].to_string();
                                    let decoded_secret = general_purpose::URL_SAFE
                                        .decode(encoded_secret)
                                        .expect("failed to decode base64 secret");
                                    let secret_utf8 = std::str::from_utf8(&decoded_secret)
                                        .expect("failed to convert base64 to string")
                                        .to_string();

                                    debug!(
                                        "{}\t{}\t{}",
                                        app_id,
                                        timezone.to_lowercase(),
                                        secret_utf8
                                    );
                                    self.secrets.insert(timezone, secret_utf8);
                                });
                        });

                        Ok(())
                    } else {
                        Err(Error::AppID)
                    }
                } else {
                    Err(Error::AppID)
                }
            } else {
                Err(Error::AppID)
            }
        } else {
            Err(Error::AppID)
        }
    }

    // Check the retrieved secrets to see which one works.
    pub async fn test_secrets(&mut self) -> Result<()> {
        let secrets = self.secrets.clone();
        debug!("testing secrets: {secrets:?}");

        for (timezone, secret) in secrets.iter() {
            let response = self
                .track_url(64868955, Some(AudioQuality::Mp3), Some(secret.to_string()))
                .await;

            if response.is_ok() {
                debug!("found good secret: {}\t{}", timezone, secret);
                let secret_string = secret.to_string();

                self.set_active_secret(secret_string);

                return Ok(());
            }
        }

        Err(Error::ActiveSecret)
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct SuccessfulResponse {
    status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, ValueEnum)]
pub enum OutputFormat {
    Json,
    Tsv,
}

#[tokio::test]
async fn can_use_methods() {
    //pretty_env_logger::init();
    use insta::assert_yaml_snapshot;

    let mut client = new(None, None, None, None)
        .await
        .expect("failed to create client");

    client.refresh().await.expect("failed to refresh config");
    client
        .login(env!("QOBUZ_USERNAME"), env!("QOBUZ_PASSWORD"))
        .await
        .expect("failed to login");
    client.test_secrets().await.expect("failed to test secrets");

    assert_yaml_snapshot!(client
    .user_playlists()
    .await
    .expect("failed to fetch user playlists"),
    {
            ".user.id" => "[id]",
            ".user.login" => "[login]",
            ".playlists.items[].users_count" => "0",
            ".playlists.items[].updated_at" => "0",
            ".playlists.total" => "0",
            ".playlists.items[].duration" => "0",
            ".playlists.items[].tracks_count" => "0",
    });
    assert_yaml_snapshot!(client
    .search_albums("a love supreme".to_string(), Some(10))
    .await
    .expect("failed to search for albums"),
    {
        ".albums.total" => "0",
        ".albums.items[].artist.albums_count" => "0",
        ".albums.items[].label.albums_count" => "0",
        ".albums.items[].purchasable_at" => "0"
    });
    assert_yaml_snapshot!(client
        .album("lhrak0dpdxcbc")
        .await
        .expect("failed to get album"));
    assert_yaml_snapshot!(client
    .search_artists("pink floyd".to_string(), Some(10))
    .await
    .expect("failed to search artists"),
    {
        ".artists.items[].albums_count" => "0"
    });
    assert_yaml_snapshot!(client
        .artist(148745, Some(10))
        .await
        .expect("failed to get artist"));
    assert_yaml_snapshot!(client.track(155999429).await.expect("failed to get track"));
    assert_yaml_snapshot!(client
        .track_url(64868955, Some(AudioQuality::HIFI96), None)
        .await
        .expect("failed to get track url"), { ".url" => "[url]" });

    // let new_playlist: Playlist = assert_ok!(
    //     client
    //         .create_playlist(
    //             "test".to_string(),
    //             false,
    //             Some("This is a description".to_string()),
    //             Some(false)
    //         )
    //         .await,
    //     "creating a new playlist"
    // );
    //
    // assert_ok!(
    //     client
    //         .playlist_add_track(new_playlist.id.to_string(), vec![155999429.to_string()])
    //         .await,
    //     "adding a track to newly created playlist"
    // );
    //
    // assert_ok!(
    //     client
    //         .playlist_delete_track(new_playlist.id.to_string(), vec![155999429.to_string()])
    //         .await,
    //     "deleting track from the newly created playlist"
    // );
    //
    // assert_ok!(
    //     client.delete_playlist(new_playlist.id.to_string()).await,
    //     "deleting the newly created playlist"
    // );
}
