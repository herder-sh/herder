//! The public API is frozen: `public-api.txt` lists it, and any change to it is deliberate.
//!
//! The surface is every public item reachable from the crate root, rendered without docs,
//! bodies or private fields. After a deliberate change run
//! `UPDATE_PUBLIC_API=1 cargo test -p herder-client-core --test public_api`; it refuses to
//! write a surface that removed or changed an item unless `CLIENT_API_VERSION` went up.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use herder_client_core::{
    CLIENT_API_VERSION, Changes, Client, NewAccount, SessionSubscription, TerminalStream,
};
use herder_protocol::{CommandBody, HostId, SessionId, TerminalId};
use syn::{Attribute, Fields, ImplItem, Item, Type, UseTree, Visibility};

const VERSION_ITEM: &str = "pub const CLIENT_API_VERSION";

#[test]
fn the_public_api_matches_its_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let snapshot = root.join("public-api.txt");
    let current = render(&surface(&root.join("src")));
    let saved = fs::read_to_string(&snapshot).unwrap_or_default();
    if current == saved {
        return;
    }
    if std::env::var_os("UPDATE_PUBLIC_API").is_none() {
        panic!(
            "the public API of herder-client-core changed. If that is deliberate, update API.md, \
             bump CLIENT_API_VERSION if anything was removed or changed, and run \
             `UPDATE_PUBLIC_API=1 cargo test -p herder-client-core --test public_api`.\n\
             --- public-api.txt\n{saved}\n+++ current\n{current}"
        );
    }
    let old = entries(&saved);
    let new = entries(&current);
    let broken: Vec<&String> = old
        .difference(&new)
        .filter(|entry| !entry.starts_with(VERSION_ITEM))
        .collect();
    assert!(
        broken.is_empty() || saved_version(&saved) != Some(CLIENT_API_VERSION),
        "these items were removed or changed, which breaks clients: bump CLIENT_API_VERSION\n\n{}",
        broken
            .iter()
            .map(|entry| entry.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    fs::write(&snapshot, current).unwrap();
}

/// The native apps hold these objects across threads and await their methods on any of them,
/// as UniFFI requires.
#[test]
fn the_objects_and_their_futures_are_send() {
    fn object<T: Send + Sync + 'static>() {}
    fn future<T: Send>(_: T) {}
    object::<Client>();
    object::<SessionSubscription>();
    object::<TerminalStream>();
    object::<Changes>();
    // Never called: it only has to compile.
    let _ = |client: Client,
             session: SessionSubscription,
             terminal: TerminalStream,
             changes: Changes| {
        let host = HostId::new("h");
        future(client.pair(String::new()));
        future(client.share());
        future(client.synced(host.clone()));
        future(client.send(
            host.clone(),
            CommandBody::Interrupt {
                session_id: SessionId::new("s"),
            },
        ));
        future(client.open_terminal(host.clone(), SessionId::new("s"), 80, 24));
        future(client.add_account(
            host.clone(),
            NewAccount {
                account_id: herder_protocol::AccountId::new("a"),
                provider: herder_protocol::Provider::Claude,
                label: None,
                config_dir: None,
            },
            80,
            24,
        ));
        future(client.attach_terminal(host, TerminalId::new("t")));
        future(session.next());
        future(terminal.next());
        future(changes.next());
    };
}

/// The snapshot's entries: items separated by blank lines.
fn entries(text: &str) -> BTreeSet<String> {
    text.split("\n\n")
        .map(|entry| entry.trim().to_owned())
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// The `CLIENT_API_VERSION` a snapshot records.
fn saved_version(text: &str) -> Option<u32> {
    let line = text.lines().find(|line| line.starts_with(VERSION_ITEM))?;
    line.rsplit('=')
        .next()?
        .trim()
        .trim_end_matches(';')
        .parse()
        .ok()
}

fn render(entries: &[String]) -> String {
    let mut text = entries.join("\n\n");
    text.push('\n');
    text
}

/// Every public item reachable from `src/lib.rs`, in source order: one rendered entry per
/// item, and per public method or trait impl. Items of a public module start with a
/// `// mod <name>` line.
fn surface(src: &Path) -> Vec<String> {
    let lib = parse(&src.join("lib.rs"));
    let mut out = Vec::new();
    let mut exported: Vec<(String, PathBuf, Option<Vec<String>>)> =
        vec![(String::new(), src.join("lib.rs"), None)];
    for item in &lib.items {
        match item {
            Item::Mod(module) if is_pub(&module.vis) => exported.push((
                format!("// mod {}\n", module.ident),
                src.join(format!("{}.rs", module.ident)),
                None,
            )),
            Item::Use(using) if is_pub(&using.vis) => {
                let UseTree::Path(path) = &using.tree else {
                    panic!("unexpected pub use");
                };
                let names = match &*path.tree {
                    UseTree::Name(name) => vec![name.ident.to_string()],
                    UseTree::Group(group) => group
                        .items
                        .iter()
                        .map(|tree| match tree {
                            UseTree::Name(name) => name.ident.to_string(),
                            _ => panic!("unexpected pub use"),
                        })
                        .collect(),
                    _ => panic!("unexpected pub use"),
                };
                exported.push((
                    String::new(),
                    src.join(format!("{}.rs", path.ident)),
                    Some(names),
                ));
            }
            _ => {}
        }
    }
    for (prefix, file, only) in exported {
        let file = parse(&file);
        let wanted = |name: &str| {
            only.as_ref()
                .is_none_or(|only| only.iter().any(|n| n == name))
        };
        let public: Vec<String> = file
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Struct(item) if is_pub(&item.vis) => Some(item.ident.to_string()),
                Item::Enum(item) if is_pub(&item.vis) => Some(item.ident.to_string()),
                _ => None,
            })
            .filter(|name| wanted(name))
            .collect();
        for item in &file.items {
            entries_of(item, &prefix, &public, &wanted, &mut out);
        }
    }
    out
}

fn entries_of(
    item: &Item,
    prefix: &str,
    public: &[String],
    wanted: &dyn Fn(&str) -> bool,
    out: &mut Vec<String>,
) {
    let mut item = item.clone();
    let entry = match &mut item {
        Item::Const(c) if is_pub(&c.vis) && wanted(&c.ident.to_string()) => {
            strip(&mut c.attrs);
            pretty(item)
        }
        Item::Fn(f) if is_pub(&f.vis) && wanted(&f.sig.ident.to_string()) => {
            strip(&mut f.attrs);
            f.block.stmts.clear();
            pretty(item)
        }
        Item::Struct(s) if is_pub(&s.vis) && wanted(&s.ident.to_string()) => {
            strip(&mut s.attrs);
            let mut opaque = false;
            match &mut s.fields {
                Fields::Named(fields) => {
                    let all = std::mem::take(&mut fields.named);
                    for mut field in all {
                        if is_pub(&field.vis) {
                            strip(&mut field.attrs);
                            fields.named.push(field);
                        } else {
                            opaque = true;
                        }
                    }
                }
                Fields::Unnamed(fields) => {
                    opaque = fields.unnamed.iter().any(|field| !is_pub(&field.vis));
                    if opaque {
                        s.fields = Fields::Named(syn::parse_quote!({}));
                        s.semi_token = None;
                    }
                }
                Fields::Unit => {}
            }
            let text = pretty(item);
            if opaque {
                format!("// opaque: private fields\n{text}")
            } else {
                text
            }
        }
        Item::Enum(e) if is_pub(&e.vis) && wanted(&e.ident.to_string()) => {
            strip(&mut e.attrs);
            for variant in &mut e.variants {
                strip(&mut variant.attrs);
                for field in variant.fields.iter_mut() {
                    strip(&mut field.attrs);
                }
            }
            pretty(item)
        }
        Item::Impl(block) => {
            let Type::Path(self_ty) = &*block.self_ty else {
                return;
            };
            let Some(name) = self_ty.path.segments.last().map(|s| s.ident.to_string()) else {
                return;
            };
            if !public.contains(&name) {
                return;
            }
            strip(&mut block.attrs);
            if block.trait_.is_some() {
                block.items.clear();
                out.push(format!("{prefix}{}", pretty(item)));
                return;
            }
            let methods = std::mem::take(&mut block.items);
            for method in methods {
                let ImplItem::Fn(mut method) = method else {
                    continue;
                };
                if !is_pub(&method.vis) {
                    continue;
                }
                strip(&mut method.attrs);
                method.block.stmts.clear();
                let mut single = block.clone();
                single.items = vec![ImplItem::Fn(method)];
                out.push(format!("{prefix}{}", pretty(Item::Impl(single))));
            }
            return;
        }
        _ => return,
    };
    out.push(format!("{prefix}{entry}"));
}

/// Keeps only the attributes that are part of the contract: derives and `non_exhaustive`.
fn strip(attrs: &mut Vec<Attribute>) {
    attrs.retain(|attr| attr.path().is_ident("derive") || attr.path().is_ident("non_exhaustive"));
}

fn is_pub(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

fn pretty(item: Item) -> String {
    prettyplease::unparse(&syn::File {
        shebang: None,
        attrs: Vec::new(),
        items: vec![item],
    })
    .trim()
    .to_owned()
}

fn parse(path: &Path) -> syn::File {
    let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    syn::parse_file(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}
