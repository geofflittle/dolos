// A parameter update that gained a field neither key list names, unless the
// probe is built with the cfg complete.
pub struct Update {
    pub minfee_a: Option<u64>,
    pub peras_cert_boost: Option<u64>,
    #[cfg(not(complete))]
    pub new_key: Option<u64>,
}

// The hand list of the judge at the base, by field access.
#[cfg(base)]
macro_rules! dijkstra_keys {
    ($update:expr) => {
        dijkstra_keys!($update; peras_cert_boost)
    };
    ($update:expr; $($name:ident),+) => {{
        let update = $update;
        let mut keys = vec![];
        $(if update.$name.is_some() {
            keys.push(stringify!($name));
        })+
        keys
    }};
}

// The destructure of the judge on the branch, every field named.
#[cfg(not(base))]
macro_rules! dijkstra_keys {
    (@ledger $update:expr; [$($conway:ident),+]; $($name:ident),+) => {{
        let Update {
            $($conway: _,)+
            $($name,)+
        }: &Update = &$update;
        let mut keys = vec![];
        $(if $name.is_some() {
            keys.push(stringify!($name));
        })+
        keys
    }};
    ($side:ident $update:expr) => {
        dijkstra_keys!(@$side $update; [minfee_a]; peras_cert_boost)
    };
}

fn main() {
    let update = Update {
        minfee_a: None,
        peras_cert_boost: Some(7),
        #[cfg(not(complete))]
        new_key: Some(1),
    };
    #[cfg(base)]
    let keys = dijkstra_keys!(&update);
    #[cfg(not(base))]
    let keys = dijkstra_keys!(ledger update);
    println!("KEYS {keys:?}");
}
