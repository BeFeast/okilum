use super::*;
use x11rb::{
    connection::Connection,
    protocol::{
        res::{ClientIdMask, ClientIdSpec, ConnectionExt as _},
        xproto::{ConnectionExt as _, ImageFormat, ImageOrder, MapState},
    },
    rust_connection::RustConnection,
};

fn wayland_session(display_present: bool, session_type: Option<&str>) -> bool {
    display_present || session_type.is_some_and(|s| s.eq_ignore_ascii_case("wayland"))
}

pub(super) fn capture() -> Result<PngBytes, Unsupported> {
    // XWayland is deliberately excluded, even when DISPLAY is also set.
    if wayland_session(
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
    ) {
        return Err(Unsupported::Backend);
    }
    let (conn, _) = RustConnection::connect(None).map_err(|_| Unsupported::Backend)?;
    let (w, h, pixels) = read_window(&conn)?;
    encode(w, h, &pixels)
}
fn own(conn: &RustConnection, window: u32) -> Result<bool, Unsupported> {
    let ids = conn
        .res_query_client_ids(&[ClientIdSpec {
            client: window,
            mask: ClientIdMask::LOCAL_CLIENT_PID,
        }])
        .map_err(|_| Unsupported::Backend)?
        .reply()
        .map_err(|_| Unsupported::Backend)?;
    // XRes supplies the actual server-side client's PID; a client-set
    // _NET_WM_PID property is not ownership evidence.
    Ok(ids.ids.iter().any(|id| id.value == [std::process::id()]))
}
fn read_window(conn: &RustConnection) -> Result<(u32, u32, Vec<u8>), Unsupported> {
    fn fail<T>(_: T) -> Unsupported {
        Unsupported::CaptureFailed
    }
    let mut window = conn
        .get_input_focus()
        .map_err(fail)?
        .reply()
        .map_err(fail)?
        .focus;
    if window <= 1 || !own(conn, window)? {
        return Err(Unsupported::NoOwnWindow);
    }
    // A focused input child is not the entire app window. Stop at the WM's
    // frame, whose resource belongs to another client, never at the root.
    for _ in 0..64 {
        let tree = conn
            .query_tree(window)
            .map_err(fail)?
            .reply()
            .map_err(fail)?;
        if tree.parent == tree.root || tree.parent == 0 || !own(conn, tree.parent)? {
            break;
        }
        window = tree.parent;
    }
    read_owned_window(conn, window)
}

fn read_owned_window(
    conn: &RustConnection,
    window: u32,
) -> Result<(u32, u32, Vec<u8>), Unsupported> {
    fn fail<T>(_: T) -> Unsupported {
        Unsupported::CaptureFailed
    }
    if window <= 1 || !own(conn, window)? {
        return Err(Unsupported::NoOwnWindow);
    }
    let attrs = conn
        .get_window_attributes(window)
        .map_err(fail)?
        .reply()
        .map_err(fail)?;
    if attrs.map_state != MapState::VIEWABLE {
        return Err(Unsupported::NoOwnWindow);
    }
    let geom = conn
        .get_geometry(window)
        .map_err(fail)?
        .reply()
        .map_err(fail)?;
    let (width, height) = (u32::from(geom.width), u32::from(geom.height));
    let len = dimensions(width, height)?;
    // Do not freeze the X server. Check actual client ownership at the read
    // boundary and discard the image if the resource disappears or changes.
    if !own(conn, window)? {
        return Err(Unsupported::NoOwnWindow);
    }
    let image = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            window,
            0,
            0,
            geom.width,
            geom.height,
            u32::MAX,
        )
        .map_err(fail)?
        .reply()
        .map_err(fail)?;
    if !own(conn, window)? {
        return Err(Unsupported::NoOwnWindow);
    }
    let after = conn
        .get_window_attributes(window)
        .map_err(fail)?
        .reply()
        .map_err(fail)?;
    if after.map_state != MapState::VIEWABLE || after.visual != attrs.visual {
        return Err(Unsupported::NoOwnWindow);
    }
    let after_geometry = conn
        .get_geometry(window)
        .map_err(fail)?
        .reply()
        .map_err(fail)?;
    if after_geometry.width != geom.width
        || after_geometry.height != geom.height
        || after_geometry.depth != geom.depth
        || after_geometry.root != geom.root
    {
        return Err(Unsupported::CaptureFailed);
    }
    let setup = conn.setup();
    let format = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == image.depth)
        .ok_or(Unsupported::Backend)?;
    let visual = setup
        .roots
        .iter()
        .flat_map(|r| &r.allowed_depths)
        .flat_map(|d| &d.visuals)
        .find(|v| v.visual_id == image.visual)
        .ok_or(Unsupported::Backend)?;
    if ![16, 24, 32].contains(&format.bits_per_pixel) || format.scanline_pad == 0 {
        return Err(Unsupported::Backend);
    }
    let stride = (width as usize * format.bits_per_pixel as usize)
        .div_ceil(format.scanline_pad as usize)
        * format.scanline_pad as usize
        / 8;
    if image.data.len() < stride * height as usize {
        return Err(Unsupported::CaptureFailed);
    }
    let mut rgba = Vec::with_capacity(len);
    for row in image.data.chunks(stride).take(height as usize) {
        for bytes in row
            .chunks(format.bits_per_pixel as usize / 8)
            .take(width as usize)
        {
            let mut pixel = 0u32;
            if setup.image_byte_order == ImageOrder::LSB_FIRST {
                for (i, byte) in bytes.iter().enumerate() {
                    pixel |= u32::from(*byte) << (i * 8);
                }
            } else {
                for byte in bytes {
                    pixel = (pixel << 8) | u32::from(*byte);
                }
            }
            for mask in [visual.red_mask, visual.green_mask, visual.blue_mask] {
                if mask == 0 {
                    return Err(Unsupported::Backend);
                }
                let max = mask >> mask.trailing_zeros();
                rgba.push(
                    (((pixel & mask) >> mask.trailing_zeros()) as u64 * 255 / u64::from(max)) as u8,
                );
            }
            rgba.push(255);
        }
    }
    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::{
        ConfigureWindowAux, CreateGCAux, CreateWindowAux, InputFocus, Rectangle, WindowClass,
    };
    #[test]
    fn wayland_and_xwayland_are_attach_only() {
        assert!(wayland_session(true, Some("x11")));
        assert!(wayland_session(false, Some("wayland")));
        assert!(!wayland_session(false, Some("x11")));
        assert!(!wayland_session(false, None));
    }
    /// Run on an isolated Xvfb server, never on an operator's desktop.
    #[test]
    #[ignore = "requires an isolated X11 server with XRes 1.2"]
    fn native_x11_pixels_and_foreign_window_rejection() {
        let (conn, screen) = RustConnection::connect(None).unwrap();
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().unwrap();
        let gc = conn.generate_id().unwrap();
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            80,
            60,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(0x123456)
                .override_redirect(1),
        )
        .unwrap()
        .check()
        .unwrap();
        conn.map_window(window).unwrap().check().unwrap();
        conn.create_gc(gc, window, &CreateGCAux::new().foreground(0x123456))
            .unwrap()
            .check()
            .unwrap();
        conn.poly_fill_rectangle(
            window,
            gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: 80,
                height: 60,
            }],
        )
        .unwrap()
        .check()
        .unwrap();
        conn.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        let png = capture().expect("positive control: native XGetImage must supply pixels");
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (80, 60));
        assert_eq!(decoded.get_pixel(40, 30).0, [0x12, 0x34, 0x56, 255]);
        conn.configure_window(window, &ConfigureWindowAux::new().width(100).height(70))
            .unwrap()
            .check()
            .unwrap();
        conn.poly_fill_rectangle(
            window,
            gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: 100,
                height: 70,
            }],
        )
        .unwrap()
        .check()
        .unwrap();
        let resized = capture().expect("resized native window must use its current dimensions");
        assert_eq!(
            image::load_from_memory(&resized)
                .unwrap()
                .to_rgba8()
                .dimensions(),
            (100, 70)
        );
        // Root belongs to the server, not this client. It can never be captured.
        assert!(!own(&conn, root).unwrap());
        conn.set_input_focus(InputFocus::POINTER_ROOT, root, x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(capture(), Err(Unsupported::NoOwnWindow));
        conn.destroy_window(window).unwrap().check().unwrap();
        assert!(
            read_owned_window(&conn, window).is_err(),
            "a destroyed target must not produce PNG bytes"
        );
    }
}
