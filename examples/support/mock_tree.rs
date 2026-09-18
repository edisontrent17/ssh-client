//! Deterministic in-memory fixtures. No filesystem or network operations.
use crate::files::{Entry, Event, Request, Worker};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::AtomicBool, mpsc},
};

pub fn listings(count: usize, nested: bool) -> BTreeMap<String, Vec<Entry>> {
    let mut directories = BTreeMap::<String, Vec<Entry>>::new();
    let mut root = vec![];
    for index in 0..count {
        let name = if index % 101 == 0 {
            format!("target-{index:06}.log")
        } else {
            format!("file-{index:06}.txt")
        };
        let parent = if nested {
            format!("/mock/group-{:04}", index / 100)
        } else {
            "/mock".into()
        };
        let entry = Entry {
            path: format!("{parent}/{name}"),
            name,
            directory: false,
            file: true,
            size: 4096,
        };
        if nested {
            if index % 100 == 0 {
                root.push(Entry {
                    path: parent.clone(),
                    name: format!("group-{:04}", index / 100),
                    directory: true,
                    file: false,
                    size: 0,
                });
            }
            directories.entry(parent).or_default().push(entry);
        } else {
            root.push(entry);
        }
    }
    directories.insert("/mock".into(), root);
    directories
}

pub fn worker(ctx: eframe::egui::Context, directories: BTreeMap<String, Vec<Entry>>) -> Worker {
    let (sender, requests) = mpsc::channel();
    let (events, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let list = |path: &str| {
            directories
                .get(path)
                .cloned()
                .ok_or_else(|| "Mock folder is not present.".to_owned())
        };
        while let Ok(request) = requests.recv() {
            let event = match request {
                Request::List(path) => match list(&path) {
                    Ok(entries) => Event::Entries(path, entries),
                    Err(error) => Event::ListFailed { path, error },
                },
                Request::Refresh { root, expanded } => {
                    let mut paths = BTreeSet::from([root.clone()]);
                    paths.extend(expanded.into_iter().filter(|p| {
                        directories.contains_key(p) && p.starts_with(&format!("{root}/"))
                    }));
                    Event::Refreshed(
                        paths
                            .into_iter()
                            .map(|p| {
                                let result = list(&p);
                                (p, result)
                            })
                            .collect(),
                    )
                }
                Request::Preview { id, remote, offset } => {
                    let text = format!(
                        "Mock file: {remote}\n\nThis content is generated in memory.\nNo SSH connection or local download is used.\n"
                    );
                    let bytes = text.as_bytes();
                    let start = usize::try_from(offset)
                        .unwrap_or(usize::MAX)
                        .min(bytes.len());
                    Event::Preview {
                        id,
                        result: Ok(crate::preview::Chunk {
                            offset,
                            size: bytes.len() as u64,
                            bytes: bytes[start..].to_vec(),
                        }),
                    }
                }
                Request::ClosePreview => continue,
                _ => Event::Error("This demo supports only in-memory browsing and preview.".into()),
            };
            if events.send(event).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    Worker {
        sender,
        receiver,
        cancel: Arc::new(AtomicBool::new(false)),
    }
}
