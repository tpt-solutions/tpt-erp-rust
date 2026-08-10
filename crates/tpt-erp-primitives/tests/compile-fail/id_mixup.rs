use tpt_erp_primitives::{Entity, Id};

struct User;
impl Entity for User {}

struct Product;
impl Entity for Product {}

fn takes_user(_: Id<User>) {}

fn main() {
    let product: Id<Product> = Id::new();
    // A Product id must not be accepted where a User id is expected.
    takes_user(product);
}
