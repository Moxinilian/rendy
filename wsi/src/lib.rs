

#[cfg(feature = "metal")]
mod gfx_backend_metal {

pub(super) fn create_surface(instance: &gfx_backend_metal::Instance, window: &winit::Window) -> gfx_backend_metal::Surface {
    let nsview = winit::os::macos::WindowExt::get_nsview(window);
    instance.create_surface_from_nsview(nsview)
}

}

#[cfg(feature = "vulkan")]
mod gfx_backend_vulkan {
    pub(super) fn create_surface(instance: &gfx_backend_vulkan::Instance, window: &winit::Window) -> <gfx_backend_vulkan::Backend as gfx_hal::Backend>::Surface {
        instance.create_surface(window)
    }
}

macro_rules! create_surface_for_backend {
    (match $instance:ident, $window:ident $(| $backend:ident @ $feature:meta)+) => {{
        #[allow(non_camel_case_types)]
        enum _B {$(
            $backend,
        )+}

        for b in [$(_B::$backend),+].iter() {
            match b {$(
                #[$feature]
                _B::$backend => {
                    if let Some(instance) = std::any::Any::downcast_ref(&**$instance) {
                        let surface: Box<std::any::Any> = Box::new(self::$backend::create_surface(instance, $window));
                        return *surface.downcast().expect(concat!("`", stringify!($backend), "::Backend::Surface` must be `", stringify!($backend), "::Surface`"));
                    }
                })+
                _ => continue,
            }
        }
        panic!("
            Undefined backend requested.
            Make sure feature for required backend is enabled.
            Try to add `--features=vulkan` or if on macos `--features=metal`.
        ")
    }};

    ($instance:ident, $window:ident) => {{
        create_surface_for_backend!(match $instance, $window
            | gfx_backend_dx12 @ cfg(feature = "dx12")
            | gfx_backend_metal @ cfg(feature = "metal")
            | gfx_backend_vulkan @ cfg(feature = "vulkan")
        );
    }};
}

#[allow(unused_variables)]
fn create_surface<B: gfx_hal::Backend>(instance: &Box<dyn std::any::Any>, window: &winit::Window) -> B::Surface {
    create_surface_for_backend!(instance, window);
}

/// Rendering target bound to window.
#[derive(derivative::Derivative)]
#[derivative(Debug)]
pub struct Target<B: gfx_hal::Backend> {
    #[derivative(Debug = "ignore")] window: winit::Window,
    #[derivative(Debug = "ignore")] surface: B::Surface,
    #[derivative(Debug = "ignore")] swapchain: B::Swapchain,
    images: Vec<B::Image>,
    format: gfx_hal::format::Format,
    extent: gfx_hal::window::Extent2D,
    usage: gfx_hal::image::Usage,
    relevant: relevant::Relevant,
}

impl<B> Target<B>
where
    B: gfx_hal::Backend,
{
    pub fn new(
        instance: &Box<dyn std::any::Any>,
        physical_device: &B::PhysicalDevice,
        device: &impl gfx_hal::Device<B>,
        window: winit::Window,
        image_count: u32,
        usage: gfx_hal::image::Usage,
    ) -> Result<Self, failure::Error> {
        let mut surface = create_surface::<B>(instance, &window);

        let (capabilities, formats, present_modes) = gfx_hal::Surface::compatibility(&surface, physical_device);

        let present_mode = *present_modes.iter().max_by_key(|mode| match mode {
            gfx_hal::PresentMode::Immediate => 0,
            gfx_hal::PresentMode::Mailbox => 3,
            gfx_hal::PresentMode::Fifo => 2,
            gfx_hal::PresentMode::Relaxed => 1,
        }).unwrap();

        log::info!("Surface present modes: {:#?}. Pick {:#?}", present_modes, present_mode);

        let formats = formats.unwrap();

        let format = *formats.iter().max_by_key(|format| {
            let base = format.base_format();
            let desc = base.0.desc();
            (!desc.is_compressed(), desc.bits, base.1 == gfx_hal::format::ChannelType::Srgb)
        }).unwrap();

        log::info!("Surface formats: {:#?}. Pick {:#?}", formats, format);

        let image_count = image_count
            .min(capabilities.image_count.end)
            .max(capabilities.image_count.start);

        log::info!("Surface capabilities: {:#?}. Pick {} images", capabilities.image_count, image_count);
        assert!(capabilities.usage.contains(usage));

        let (swapchain, backbuffer) = device.create_swapchain(
            &mut surface,
            gfx_hal::SwapchainConfig {
                present_mode,
                format,
                extent: capabilities.current_extent.unwrap(),
                image_count,
                image_layers: 1,
                image_usage: usage,
            },
            None,
        )?;

        let images = if let gfx_hal::Backbuffer::Images(images) = backbuffer {
            images
        } else {
            panic!("Framebuffer backbuffer is not supported");
        };

        Ok(Target {
            window,
            surface,
            swapchain,
            images,
            format,
            extent: capabilities.current_extent.unwrap(),
            usage,
            relevant: relevant::Relevant,
        })
    }

    /// Strip the target to the internal parts.
    ///
    /// # Safety
    ///
    /// Swapchain must be not in use.
    pub unsafe fn dispose(self, device: &impl gfx_hal::Device<B>) -> winit::Window {
        device.destroy_swapchain(self.swapchain);
        drop(self.surface);
        self.relevant.dispose();
        self.window
    }

    /// Get raw surface handle.
    pub fn surface(&self) -> &B::Surface {
        &self.surface
    }

    /// Get raw surface handle.
    pub fn swapchain(&self) -> &B::Swapchain {
        &self.swapchain
    }

    /// Get swapchain impl trait.
    ///
    /// # Safety
    ///
    /// Trait usage should not violate this type valid usage.
    pub unsafe fn swapchain_mut(&mut self) -> &mut impl gfx_hal::Swapchain<B> {
        &mut self.swapchain
    }

    /// Get target current extent.
    pub fn extent(&self) -> gfx_hal::window::Extent2D {
        self.extent
    }

    /// Get target current format.
    pub fn format(&self) -> gfx_hal::format::Format {
        self.format
    }

    /// Get raw handlers for the swapchain images.
    pub fn images(&self) -> &[B::Image] {
        &self.images
    }

    pub fn image_info(&self) -> rendy_resource::image::Info {
        rendy_resource::image::Info {
            kind: gfx_hal::Surface::kind(&self.surface),
            levels: 1,
            format: self.format,
            tiling: gfx_hal::image::Tiling::Optimal,
            view_caps: gfx_hal::image::ViewCapabilities::empty(),
            usage: self.usage,
        }
    }

    /// Acquire next image.
    pub fn next_image(&mut self, signal: &B::Semaphore) -> Result<NextImages<'_, B>, gfx_hal::AcquireError> {
        let index = unsafe {
            gfx_hal::Swapchain::acquire_image(&mut self.swapchain, !0, gfx_hal::FrameSync::Semaphore(signal))
        }?;

        Ok(NextImages {
            swapchains: std::iter::once((&self.swapchain, index)).collect(),
        })
    }
}

#[derive(derivative::Derivative)]
#[derivative(Debug)]
pub struct NextImages<'a, B: gfx_hal::Backend> {
    #[derivative(Debug = "ignore")]
    swapchains: smallvec::SmallVec<[(&'a B::Swapchain, u32); 8]>,
}

impl<'a, B> NextImages<'a, B>
where
    B: gfx_hal::Backend,
{
    /// Get indices.
    pub fn indices(&self) -> impl IntoIterator<Item = u32> + '_ {
        self.swapchains.iter().map(|(_s, i)| *i)
    }

    /// Present images by the queue.
    ///
    /// # TODO
    ///
    /// Use specific presentation error type.
    pub fn present(self, queue: &mut impl gfx_hal::queue::RawCommandQueue<B>, wait: &[B::Semaphore]) -> Result<(), failure::Error> {
        unsafe {
            queue.present(
                self.swapchains.iter().cloned(),
                wait,
            ).map_err(|()| failure::format_err!("Suboptimal or out of date?"))
        }
    }
}
