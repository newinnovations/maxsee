use chrono::Local;
use gdk_pixbuf::PixbufLoader;
use gtk::gdk;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, Entry, Label, Orientation, Picture,
};
use gtk4 as gtk;
use microscope::{Frame, Microscope, MicroscopeConfig};
use std::cell::RefCell;
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::time::Duration;

fn main() {
    let app = Application::builder()
        .application_id("org.omp.WifiMicroscope")
        .build();
    app.connect_activate(build_window);
    app.run();
}

fn build_window(app: &Application) {
    let device_ip = Entry::builder().text("192.168.29.1").hexpand(true).build();
    let connect_button = Button::with_label("Connect");
    let capture_button = Button::with_label("Capture JPEG");
    capture_button.set_sensitive(false);
    let status = Label::new(Some("Connect to the microscope Wi-Fi network first."));
    let preview = Picture::new();
    preview.set_hexpand(true);
    preview.set_vexpand(true);

    let controls = GtkBox::new(Orientation::Horizontal, 8);
    controls.append(&Label::new(Some("Microscope IPv4:")));
    controls.append(&device_ip);
    controls.append(&connect_button);
    controls.append(&capture_button);

    let layout = GtkBox::new(Orientation::Vertical, 8);
    layout.set_margin_top(12);
    layout.set_margin_bottom(12);
    layout.set_margin_start(12);
    layout.set_margin_end(12);
    layout.append(&controls);
    layout.append(&preview);
    layout.append(&status);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Wi-Fi Microscope")
        .default_width(960)
        .default_height(720)
        .child(&layout)
        .build();

    let camera = Rc::new(RefCell::new(None::<Microscope>));
    let latest_frame = Rc::new(RefCell::new(None::<Frame>));

    {
        let camera = Rc::clone(&camera);
        let latest_frame = Rc::clone(&latest_frame);
        let device_ip = device_ip.clone();
        let status = status.clone();
        let capture_button = capture_button.clone();
        let preview = preview.clone();
        let connect_button = connect_button.clone();
        let clicked_button = connect_button.clone();
        connect_button.connect_clicked(move |_| {
            if camera.borrow().is_some() {
                camera.borrow_mut().take();
                latest_frame.borrow_mut().take();
                preview.set_paintable(None::<&gdk::Paintable>);
                capture_button.set_sensitive(false);
                clicked_button.set_label("Connect");
                status.set_text("Disconnected.");
                return;
            }

            let address = match device_ip.text().parse::<Ipv4Addr>() {
                Ok(address) => address,
                Err(error) => {
                    status.set_text(&format!("Invalid IPv4 address: {error}"));
                    return;
                }
            };
            match Microscope::connect(MicroscopeConfig::new(address)) {
                Ok(connection) => {
                    *camera.borrow_mut() = Some(connection);
                    clicked_button.set_label("Disconnect");
                    status.set_text(&format!("Connected to {address}; waiting for frames."));
                }
                Err(error) => status.set_text(&format!("Could not connect: {error}")),
            }
        });
    }

    {
        let latest_frame = Rc::clone(&latest_frame);
        let status = status.clone();
        capture_button.connect_clicked(move |_| {
            let latest = latest_frame.borrow();
            let Some(frame) = latest.as_ref() else {
                status.set_text("No complete frame is available yet.");
                return;
            };
            let filename = format!(
                "microscope_{}.jpg",
                Local::now().format("%Y%m%d_%H%M%S_%3f")
            );
            match frame.save_jpeg(&filename) {
                Ok(()) => {
                    status.set_text(&format!("Saved frame {} to {filename}.", frame.sequence))
                }
                Err(error) => status.set_text(&format!("Could not save image: {error}")),
            }
        });
    }

    {
        let camera = Rc::clone(&camera);
        let latest_frame = Rc::clone(&latest_frame);
        let preview = preview.clone();
        let status = status.clone();
        let connect_button = connect_button.clone();
        let capture_button = capture_button.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(15), move || {
            let poll_result = {
                let mut camera = camera.borrow_mut();
                camera.as_mut().map(Microscope::poll)
            };
            match poll_result {
                Some(Ok(Some(frame))) => match load_texture(&frame.jpeg) {
                    Ok(texture) => {
                        preview.set_paintable(Some(&texture));
                        status.set_text(&format!("Live frame {}.", frame.sequence));
                        *latest_frame.borrow_mut() = Some(frame);
                        capture_button.set_sensitive(true);
                    }
                    Err(error) => status.set_text(&format!("Could not decode JPEG: {error}")),
                },
                Some(Err(error)) => {
                    camera.borrow_mut().take();
                    latest_frame.borrow_mut().take();
                    capture_button.set_sensitive(false);
                    connect_button.set_label("Connect");
                    status.set_text(&format!("Connection stopped: {error}"));
                }
                Some(Ok(None)) | None => {}
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    window.present();
}

fn load_texture(jpeg: &[u8]) -> Result<gdk::Texture, String> {
    let loader = PixbufLoader::new();
    loader.write(jpeg).map_err(|error| error.to_string())?;
    loader.close().map_err(|error| error.to_string())?;
    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| "JPEG decoder produced no image".to_string())?;
    Ok(gdk::Texture::for_pixbuf(&pixbuf))
}
