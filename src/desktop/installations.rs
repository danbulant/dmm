use std::path::Path;

use anyhow::Result;
use dmm::games::GameKind;
use dmm::library::{Instance, Library};

use super::DetectedGame;

pub(super) enum Installation<'a> {
    Registered {
        instance: &'a Instance,
        detected: bool,
    },
    Detected(&'a Path),
}

impl Installation<'_> {
    pub(super) fn root(&self) -> &Path {
        match self {
            Self::Registered { instance, .. } => &instance.root,
            Self::Detected(root) => root,
        }
    }

    fn use_in(self, kind: GameKind, library: &Library) -> Result<String> {
        let name = match self {
            Self::Registered { instance, .. } => instance.name.clone(),
            Self::Detected(root) => {
                let instances = library.instances()?;
                let base = kind.to_string();
                let name = if instances.iter().any(|i| i.name == base) {
                    (2..)
                        .map(|n| format!("{base}-{n}"))
                        .find(|name| !instances.iter().any(|i| &i.name == name))
                        .expect("finite set of registered names")
                } else {
                    base
                };
                library.register(&name, kind, root, None)?.name
            }
        };
        library.use_installation(&name)?;
        Ok(name)
    }
}

pub(super) fn candidates<'a>(
    instances: &'a [Instance],
    detected: &'a [DetectedGame],
    kind: GameKind,
) -> impl Iterator<Item = Installation<'a>> {
    let same_kind = instances.iter().filter(move |i| i.kind == kind);
    let found = detected.iter().filter(move |i| i.kind == kind);
    same_kind
        .clone()
        .map({
            let found = found.clone();
            move |instance| Installation::Registered {
                instance,
                detected: found.clone().any(|i| i.root == instance.root),
            }
        })
        .chain(found.filter_map(move |i| {
            (!same_kind.clone().any(|saved| saved.root == i.root))
                .then_some(Installation::Detected(i.root.as_path()))
        }))
}

pub(super) fn resolve(
    library: &Library,
    detected: &[DetectedGame],
    kind: GameKind,
) -> Result<Option<String>> {
    let instances = library.instances()?;
    if let Some(name) = library.active_installation(kind)?
        && instances
            .iter()
            .any(|i| i.name == name && i.kind == kind && valid(i))
    {
        return Ok(Some(name));
    }
    let mut matches = candidates(&instances, detected, kind).filter(|item| match item {
        Installation::Registered { instance, .. } => valid(instance),
        Installation::Detected(root) => root.is_dir(),
    });
    let first = matches.next();
    if matches.next().is_some() {
        return Ok(None);
    }
    first.map(|item| item.use_in(kind, library)).transpose()
}

pub(super) fn select(library: &Library, kind: GameKind, root: &Path) -> Result<String> {
    let instances = library.instances()?;
    if let Some(instance) = instances.iter().find(|i| i.kind == kind && i.root == root) {
        Installation::Registered {
            instance,
            detected: false,
        }
        .use_in(kind, library)
    } else {
        Installation::Detected(root).use_in(kind, library)
    }
}

fn valid(instance: &Instance) -> bool {
    instance.root.is_dir() && instance.profile().is_ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn a_single_detected_installation_becomes_the_persisted_default() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("game");
        fs::create_dir(&root).unwrap();
        let db = fixture.path().join("dmm");
        let library = Library::open(&db).unwrap();
        let found = [DetectedGame {
            kind: GameKind::Cyberpunk,
            root: root.canonicalize().unwrap(),
        }];
        let name = resolve(&library, &found, GameKind::Cyberpunk)
            .unwrap()
            .unwrap();
        let registered = library.instances().unwrap();
        assert_eq!(
            candidates(&registered, &found, GameKind::Cyberpunk).count(),
            1
        );
        assert_eq!(library.instance(&name).unwrap().root, found[0].root);
        drop(library);
        let library = Library::open(&db).unwrap();
        assert_eq!(
            resolve(&library, &[], GameKind::Cyberpunk).unwrap(),
            Some(name)
        );
        assert_eq!(resolve(&library, &[], GameKind::Zzmi).unwrap(), None);
    }

    #[test]
    fn several_installations_wait_for_a_choice_and_remember_it() {
        let fixture = tempfile::tempdir().unwrap();
        let library = Library::open(&fixture.path().join("dmm")).unwrap();
        let found = ["first", "second"].map(|name| {
            let root = fixture.path().join(name);
            fs::create_dir(&root).unwrap();
            DetectedGame {
                kind: GameKind::Cyberpunk,
                root: root.canonicalize().unwrap(),
            }
        });
        assert_eq!(
            resolve(&library, &found, GameKind::Cyberpunk).unwrap(),
            None
        );
        let picked = select(&library, GameKind::Cyberpunk, &found[1].root).unwrap();
        assert_eq!(
            resolve(&library, &found, GameKind::Cyberpunk).unwrap(),
            Some(picked.clone())
        );
        assert_eq!(library.instance(&picked).unwrap().root, found[1].root);
    }
}
