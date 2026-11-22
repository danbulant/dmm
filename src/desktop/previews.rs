mod disk;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use image::{ImageReader, Limits};
use reqwest::blocking::Client;
use reqwest::header::{AGE, CACHE_CONTROL, HeaderMap};
use reqwest::redirect::Policy;
use rune::tok::Theme;
use rune::{El, Pixels, SurfaceFit, Waker, el};
use url::Url;

use super::muted;
use disk::ThumbnailCache;

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const WORKERS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Size {
    Card,
    Thumbnail,
    Gallery,
    Icon,
}

impl Size {
    fn edge(self) -> u32 {
        match self {
            Self::Card => 384,
            Self::Thumbnail => 180,
            Self::Gallery => 4096,
            Self::Icon => 48,
        }
    }
}

type ImageKey = (String, Size);

#[derive(Default)]
struct Queue {
    jobs: VecDeque<(ImageKey, bool)>,
    outstanding: HashSet<ImageKey>,
    closed: bool,
}

#[derive(Default)]
struct Dispatch {
    queue: Mutex<Queue>,
    ready: Condvar,
}

struct Preview {
    key: ImageKey,
    image: Option<Decoded>,
}

struct Decoded {
    pixels: Pixels,
    width: u32,
    height: u32,
    bytes: usize,
    expires: Instant,
}

struct Entry {
    image: Option<Decoded>,
    expires: Instant,
    touched: Instant,
}

pub(super) struct Previews {
    dispatch: Arc<Dispatch>,
    responses: Receiver<Preview>,
    images: HashMap<Size, HashMap<String, Entry>>,
    wanted: HashSet<ImageKey>,
    bytes: usize,
}

impl Drop for Previews {
    fn drop(&mut self) {
        self.dispatch.queue.lock().unwrap().closed = true;
        self.dispatch.ready.notify_all();
    }
}

impl Previews {
    #[cfg(test)]
    pub(super) fn thumbnail_fixture(url: &str) -> Self {
        let pixels = Pixels::new();
        let (width, height) = (180, 100);
        let bytes = (width * height * 4) as usize;
        pixels.set(width, height, vec![255; bytes]);
        let now = Instant::now();
        let expires = now + Duration::from_secs(60);
        let entry = Entry {
            image: Some(Decoded {
                pixels,
                width,
                height,
                bytes,
                expires,
            }),
            expires,
            touched: now,
        };
        Self {
            dispatch: Arc::new(Dispatch::default()),
            responses: mpsc::channel().1,
            images: HashMap::from([(Size::Thumbnail, HashMap::from([(url.to_owned(), entry)]))]),
            wanted: HashSet::new(),
            bytes,
        }
    }

    pub(super) fn start(state_dir: &Path, wake: Waker) -> Result<Self> {
        let dispatch = Arc::new(Dispatch::default());
        let (results, responses) = mpsc::channel();
        let client = Client::builder()
            .user_agent(concat!("dmm/", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .timeout(Duration::from_secs(12))
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() < 4 && image_host(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("image redirect is not a supported provider")
                }
            }))
            .build()?;
        let previews = Self {
            dispatch,
            responses,
            images: HashMap::new(),
            wanted: HashSet::new(),
            bytes: 0,
        };
        for worker in 0..WORKERS {
            let dispatch = Arc::clone(&previews.dispatch);
            let results = results.clone();
            let wake = wake.clone();
            let client = client.clone();
            let state_dir = state_dir.to_owned();
            thread::Builder::new()
                .name(format!("dmm-previews-{worker}"))
                .spawn(move || {
                    let cache = ThumbnailCache::open(&state_dir).ok();
                    loop {
                        let (key, saved) = {
                            let queue = dispatch.queue.lock().unwrap();
                            let mut queue = dispatch
                                .ready
                                .wait_while(queue, |q| !q.closed && q.jobs.is_empty())
                                .unwrap();
                            if queue.closed {
                                return;
                            }
                            queue.jobs.pop_front().expect("ready image job")
                        };
                        let image = load(&client, cache.as_ref(), &key.0, key.1, saved).ok();
                        if results.send(Preview { key, image }).is_err() {
                            return;
                        }
                        wake();
                    }
                })?;
        }
        Ok(previews)
    }

    pub(super) fn show<'a>(&mut self, urls: impl IntoIterator<Item = (&'a str, Size)>) {
        self.show_with_policy(urls, false);
    }

    pub(super) fn show_saved<'a>(&mut self, urls: impl IntoIterator<Item = (&'a str, Size)>) {
        self.show_with_policy(urls, true);
    }

    fn show_with_policy<'a>(
        &mut self,
        urls: impl IntoIterator<Item = (&'a str, Size)>,
        saved: bool,
    ) {
        let now = Instant::now();
        let mut queue = self.dispatch.queue.lock().unwrap();
        // Replace unstarted jobs in priority order; never duplicate running requests.
        while let Some((key, _)) = queue.jobs.pop_front() {
            queue.outstanding.remove(&key);
        }
        let old = std::mem::take(&mut self.wanted);
        for (url, size) in urls.into_iter().take(220) {
            let key = (url.to_owned(), size);
            if !self.wanted.insert(key.clone()) {
                continue;
            }
            if let Some(entry) = self
                .images
                .get_mut(&size)
                .and_then(|group| group.get_mut(url))
                && (entry.expires > now || (entry.image.is_some() && old.contains(&key)))
            {
                entry.touched = now;
                continue;
            }
            if queue.outstanding.insert(key.clone()) {
                queue.jobs.push_back((key.clone(), saved));
            }
        }
        let ready = !queue.jobs.is_empty();
        drop(queue);
        if ready {
            self.dispatch.ready.notify_all();
        }
    }

    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        let now = Instant::now();
        for reply in self.responses.try_iter() {
            self.dispatch
                .queue
                .lock()
                .unwrap()
                .outstanding
                .remove(&reply.key);
            let visible = self.wanted.contains(&reply.key);
            if !visible && reply.image.as_ref().is_none_or(|img| img.expires <= now) {
                continue;
            }
            let entry = Entry {
                expires: reply
                    .image
                    .as_ref()
                    .map_or(now + Duration::from_secs(15), |img| img.expires),
                touched: now,
                image: reply.image,
            };
            self.bytes += entry.image.as_ref().map_or(0, |img| img.bytes);
            if let Some(old) = self
                .images
                .entry(reply.key.1)
                .or_default()
                .insert(reply.key.0, entry)
            {
                self.bytes -= old.image.map_or(0, |img| img.bytes);
            }
            changed |= visible;
        }
        self.trim();
        changed
    }

    fn trim(&mut self) {
        const BUDGET: usize = 48 * 1024 * 1024;
        const MAX_ENTRIES: usize = 440;
        let mut count = self.images.values().map(HashMap::len).sum::<usize>();
        if self.bytes <= BUDGET && count <= MAX_ENTRIES {
            return;
        }
        let mut oldest: Vec<_> = self
            .images
            .iter()
            .flat_map(|(&size, images)| {
                images
                    .iter()
                    .map(move |(url, entry)| ((url.clone(), size), entry.touched))
            })
            .filter(|(key, _)| !self.wanted.contains(key))
            .collect();
        oldest.sort_unstable_by_key(|(_, touched)| *touched);
        for ((url, size), _) in oldest {
            if self.bytes <= BUDGET && count <= MAX_ENTRIES {
                break;
            }
            if let Some(entry) = self
                .images
                .get_mut(&size)
                .and_then(|images| images.remove(&url))
            {
                self.bytes -= entry.image.map_or(0, |image| image.bytes);
                count -= 1;
            }
        }
    }

    fn image(&self, size: Size, url: Option<&str>) -> Option<&Decoded> {
        self.images.get(&size)?.get(url?)?.image.as_ref()
    }

    pub(super) fn decoded(&self, size: Size, url: Option<&str>) -> Option<(&Pixels, [f32; 2])> {
        let image = self.image(size, url)?;
        let scale = if size == Size::Thumbnail {
            1920.0 / image.width.max(image.height) as f32
        } else {
            1.0
        };
        Some((
            &image.pixels,
            [image.width as f32 * scale, image.height as f32 * scale],
        ))
    }

    pub(super) fn icon(&self, url: Option<&str>, t: Theme) -> El {
        let plate = el().size(24.0, 24.0).clip_rounded(4.0).fill(t.control_off);
        if let Some(image) = self.image(Size::Icon, url) {
            plate.pixels_fit(&image.pixels, SurfaceFit::Fit)
        } else {
            plate
        }
    }

    pub(super) fn surface(
        &self,
        url: Option<&str>,
        size: Size,
        frame: El,
        fit: SurfaceFit,
        t: Theme,
    ) -> El {
        let frame = frame.fill(t.control_off).center();
        match self.image(size, url) {
            Some(image) => frame.pixels_fit(&image.pixels, fit),
            None if size == Size::Thumbnail => frame,
            None if url.is_some() => frame.child(muted("Loading preview…", t)),
            None => frame,
        }
    }

    pub(super) fn view(&self, url: Option<&str>, height: f32, fit: SurfaceFit, t: Theme) -> El {
        self.surface(
            url,
            Size::Card,
            el().w_full().h(height).clip_rounded(9.0),
            fit,
            t,
        )
    }
}

fn image_host(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|host| {
            host.ends_with(".gamebanana.com") || host.ends_with(".nexusmods.com")
        })
}

fn freshness(headers: &HeaderMap) -> Duration {
    let mut seconds = 3600_u64;
    for control in headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
    {
        for directive in control.split(',').map(str::trim) {
            let (name, value) = directive.split_once('=').unwrap_or((directive, ""));
            if name.eq_ignore_ascii_case("no-cache") || name.eq_ignore_ascii_case("no-store") {
                return Duration::ZERO;
            }
            if name.eq_ignore_ascii_case("max-age") {
                seconds = seconds.min(value.trim_matches('"').parse().unwrap_or(0));
            }
        }
    }
    let age = headers
        .get(AGE)
        .and_then(|age| age.to_str().ok())
        .and_then(|age| age.parse().ok())
        .unwrap_or(0);
    Duration::from_secs(seconds.saturating_sub(age))
}

fn load(
    client: &Client,
    cache: Option<&ThumbnailCache>,
    raw_url: &str,
    size: Size,
    saved: bool,
) -> Result<Decoded> {
    let url = Url::parse(raw_url)?;
    ensure!(image_host(&url), "image not hosted by a mod provider");
    let cache = cache.filter(|_| matches!(size, Size::Card | Size::Thumbnail));
    if let Some((bytes, ttl)) = cache.and_then(|cache| cache.get(raw_url, saved))
        && let Ok(decoded) = decode(bytes, size, ttl, None)
    {
        return Ok(decoded);
    }
    let response = client.get(url).send()?.error_for_status()?;
    let ttl = freshness(response.headers());
    ensure!(
        response.content_length().is_none_or(|len| len <= MAX_BYTES),
        "image too large"
    );
    let mut bytes = Vec::new();
    response.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "image too large");
    decode(bytes, size, ttl, cache.map(|cache| (cache, raw_url)))
}

fn decode(
    bytes: Vec<u8>,
    size: Size,
    ttl: Duration,
    cache: Option<(&ThumbnailCache, &str)>,
) -> Result<Decoded> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let mut decoded = reader.decode()?;
    if let Some((cache, url)) = cache.filter(|_| !ttl.is_zero()) {
        let edge = Size::Card.edge();
        if decoded.width().max(decoded.height()) > edge {
            decoded = decoded.thumbnail(edge, edge);
        }
        let rgba = decoded.into_rgba8();
        let _ = cache.put(url, &rgba, ttl);
        decoded = image::DynamicImage::ImageRgba8(rgba);
    }
    let edge = size.edge();
    let rgba = if decoded.width().max(decoded.height()) > edge {
        decoded.thumbnail(edge, edge).into_rgba8()
    } else {
        decoded.into_rgba8()
    };
    let (w, h) = rgba.dimensions();
    let pixels = Pixels::new();
    let bytes = rgba.as_raw().len();
    pixels.set(w, h, rgba.into_raw());
    Ok(Decoded {
        pixels,
        width: w,
        height: h,
        bytes,
        expires: Instant::now() + ttl,
    })
}
