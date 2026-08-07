//! Internal macro for *open* string-valued enums.

/// Define an enum that serializes as a plain JSON string and tolerates values
/// this build has never heard of.
///
/// Unknown values are captured in an `Unknown(String)` variant and re-serialize
/// **verbatim**, so a relay or a log pipeline built against protocol v1 can
/// carry a v1.1 peer's new route or error code through untouched instead of
/// flattening it. This is the mechanism behind the "open enum" half of the
/// compatibility rule in the crate docs.
///
/// `#[serde(from/into = "String")]` is used rather than `#[serde(other)]`
/// because `other` only supports a unit variant and would discard the original
/// string.
/// An optional trailing `default = Variant;` clause names the [`Default`]
/// value, keeping it beside the variant list instead of in a separate `impl`
/// further down the file.
macro_rules! open_str_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),* $(,)?
        }
        $(default = $default:ident;)?
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(from = "String", into = "String")]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )*
            /// A value this build does not recognize, preserved verbatim so it
            /// survives a round trip through an older peer.
            Unknown(String),
        }

        impl $name {
            /// The exact string this value takes on the wire.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $wire, )*
                    Self::Unknown(s) => s.as_str(),
                }
            }

            /// Every variant this build knows about, excluding `Unknown`.
            #[must_use]
            pub fn known() -> &'static [Self] {
                &[ $( Self::$variant, )* ]
            }

            /// Whether this value is one this build understands.
            ///
            /// A `false` here means the peer is newer than us; callers should
            /// degrade rather than treat it as malformed input.
            #[must_use]
            pub fn is_known(&self) -> bool {
                !matches!(self, Self::Unknown(_))
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                match s.as_str() {
                    $( $wire => Self::$variant, )*
                    _ => Self::Unknown(s),
                }
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self::from(s.to_string())
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                match v {
                    $( $name::$variant => $wire.to_string(), )*
                    $name::Unknown(s) => s,
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        $(
            impl ::core::default::Default for $name {
                fn default() -> Self {
                    Self::$default
                }
            }
        )?
    };
}
