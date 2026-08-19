#[test]
fn dec_graphics_probe() {
    let mut t = dmux_vt::PaneTerm::new(20, 3, 10);
    t.advance(b"\x1b(0qqq\x1b(Bqqq \x1b(0lqk\x1b(B");
    let mut buf = dmux_compositor::CellBuffer::new(20, 3);
    t.render_into(&mut buf, dmux_compositor::Rect::new(0, 0, 20, 3));
    let row: String = (0..12).map(|c| buf.get(c, 0).ch).collect();
    println!("row0: {row:?}");
    assert_eq!(&row[..], "───qqq ┌─┐  ", "DEC special graphics must decode");
}
