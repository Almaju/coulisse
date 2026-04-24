//! Small set of hand-rolled shadcn-style primitives. Tailwind-class based,
//! no external component library — Shadcn's philosophy is "own the code",
//! so we replicate the styling directly.

mod badge;
mod card;
mod empty;
mod spinner;

pub use badge::Badge;
pub use card::{Card, CardContent, CardHeader, CardTitle};
pub use empty::Empty;
pub use spinner::Spinner;
