use spellbook::Dictionary;

fn main() {
    let aff = "SET UTF-8\n".to_string();
    let dic = "2\nhello/1\t1\nworld\n".to_string();
    let mut d = Dictionary::new();
    // spellbook parsing
    let res = d.load(&aff, &dic);
    println!("{:?}", res);
}
