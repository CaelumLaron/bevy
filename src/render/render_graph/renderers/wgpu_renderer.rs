use crate::{
    asset::{AssetStorage, Handle},
    legion::prelude::*,
    render::{
        render_graph::{
            resource_name, update_shader_assignments, BindGroup, BindType,
            DynamicUniformBufferInfo, PassDescriptor, PipelineDescriptor, PipelineLayout,
            PipelineLayoutType, RenderGraph, RenderPass, RenderPassColorAttachmentDescriptor,
            RenderPassDepthStencilAttachmentDescriptor, RenderResource, RenderResources, Renderer,
            ResourceInfo, TextureDescriptor,
        },
        Shader,
    },
};
use std::{collections::HashMap, ops::Deref};

pub struct WgpuRenderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: Option<wgpu::Surface>,
    pub encoder: Option<wgpu::CommandEncoder>,
    pub swap_chain_descriptor: wgpu::SwapChainDescriptor,
    pub render_pipelines: HashMap<Handle<PipelineDescriptor>, wgpu::RenderPipeline>,
    pub buffers: HashMap<RenderResource, wgpu::Buffer>,
    pub textures: HashMap<RenderResource, wgpu::TextureView>,
    pub resource_info: HashMap<RenderResource, ResourceInfo>,
    pub bind_groups: HashMap<u64, BindGroupInfo>,
    pub bind_group_layouts: HashMap<u64, wgpu::BindGroupLayout>,
    pub dynamic_uniform_buffer_info: HashMap<RenderResource, DynamicUniformBufferInfo>,
    pub render_resources: RenderResources,
}

impl WgpuRenderer {
    pub fn new() -> Self {
        let adapter = wgpu::Adapter::request(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::Default,
            },
            wgpu::BackendBit::PRIMARY,
        )
        .unwrap();

        let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor {
            extensions: wgpu::Extensions {
                anisotropic_filtering: false,
            },
            limits: wgpu::Limits::default(),
        });

        let swap_chain_descriptor = wgpu::SwapChainDescriptor {
            usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            width: 0,
            height: 0,
            present_mode: wgpu::PresentMode::Vsync,
        };

        WgpuRenderer {
            device,
            queue,
            surface: None,
            encoder: None,
            swap_chain_descriptor,
            render_pipelines: HashMap::new(),
            buffers: HashMap::new(),
            textures: HashMap::new(),
            resource_info: HashMap::new(),
            bind_groups: HashMap::new(),
            bind_group_layouts: HashMap::new(),
            dynamic_uniform_buffer_info: HashMap::new(),
            render_resources: RenderResources::default(),
        }
    }

    pub fn create_render_pipeline(
        render_resources: &RenderResources,
        dynamic_uniform_buffer_info: &HashMap<RenderResource, DynamicUniformBufferInfo>,
        pipeline_descriptor: &mut PipelineDescriptor,
        bind_group_layouts: &mut HashMap<u64, wgpu::BindGroupLayout>,
        device: &wgpu::Device,
        vertex_shader: &Shader,
        fragment_shader: Option<&Shader>,
    ) -> wgpu::RenderPipeline {
        let vertex_spirv = vertex_shader.get_spirv_shader(None);
        let fragment_spirv = fragment_shader.map(|f| f.get_spirv_shader(None));

        let vertex_shader_module = Self::create_shader_module(device, &vertex_spirv, None);
        let fragment_shader_module = match fragment_shader {
            Some(fragment_spirv) => Some(Self::create_shader_module(device, fragment_spirv, None)),
            None => None,
        };

        if let PipelineLayoutType::Reflected(None) = pipeline_descriptor.layout {
            let mut layouts = vec![vertex_spirv.reflect_layout().unwrap()];

            if let Some(ref fragment_spirv) = fragment_spirv {
                layouts.push(fragment_spirv.reflect_layout().unwrap());
            }

            let mut layout = PipelineLayout::from_shader_layouts(&mut layouts);

            // set each uniform binding to dynamic if there is a matching dynamic uniform buffer info
            for mut bind_group in layout.bind_groups.iter_mut() {
                bind_group.bindings = bind_group
                    .bindings
                    .iter()
                    .cloned()
                    .map(|mut binding| {
                        if let BindType::Uniform {
                            ref mut dynamic, ..
                        } = binding.bind_type
                        {
                            if let Some(resource) =
                                render_resources.get_named_resource(&binding.name)
                            {
                                if dynamic_uniform_buffer_info.contains_key(&resource) {
                                    *dynamic = true;
                                }
                            }
                        }

                        binding
                    })
                    .collect();
            }

            pipeline_descriptor.layout = PipelineLayoutType::Reflected(Some(layout));
        }

        let layout = pipeline_descriptor.get_layout_mut().unwrap();
        // println!("{:#?}", layout);
        // println!();

        // setup new bind group layouts
        for bind_group in layout.bind_groups.iter_mut() {
            let bind_group_id = bind_group.get_or_update_hash();
            if let None = bind_group_layouts.get(&bind_group_id) {
                let bind_group_layout_binding = bind_group
                    .bindings
                    .iter()
                    .map(|binding| wgpu::BindGroupLayoutBinding {
                        binding: binding.index,
                        visibility: wgpu::ShaderStage::VERTEX | wgpu::ShaderStage::FRAGMENT,
                        ty: (&binding.bind_type).into(),
                    })
                    .collect::<Vec<wgpu::BindGroupLayoutBinding>>();
                let bind_group_layout =
                    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        bindings: bind_group_layout_binding.as_slice(),
                    });

                bind_group_layouts.insert(bind_group_id, bind_group_layout);
            }
        }

        // collect bind group layout references
        let bind_group_layouts = layout
            .bind_groups
            .iter()
            .map(|bind_group| {
                let bind_group_id = bind_group.get_hash().unwrap();
                bind_group_layouts.get(&bind_group_id).unwrap()
            })
            .collect::<Vec<&wgpu::BindGroupLayout>>();

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            bind_group_layouts: bind_group_layouts.as_slice(),
        });

        let mut render_pipeline_descriptor = wgpu::RenderPipelineDescriptor {
            layout: &pipeline_layout,
            vertex_stage: wgpu::ProgrammableStageDescriptor {
                module: &vertex_shader_module,
                entry_point: "main",
            },
            fragment_stage: match fragment_shader {
                Some(_) => Some(wgpu::ProgrammableStageDescriptor {
                    entry_point: "main",
                    module: fragment_shader_module.as_ref().unwrap(),
                }),
                None => None,
            },
            rasterization_state: pipeline_descriptor.rasterization_state.clone(),
            primitive_topology: pipeline_descriptor.primitive_topology,
            color_states: &pipeline_descriptor.color_states,
            depth_stencil_state: pipeline_descriptor.depth_stencil_state.clone(),
            index_format: pipeline_descriptor.index_format,
            vertex_buffers: &pipeline_descriptor
                .vertex_buffer_descriptors
                .iter()
                .map(|v| v.into())
                .collect::<Vec<wgpu::VertexBufferDescriptor>>(),
            sample_count: pipeline_descriptor.sample_count,
            sample_mask: pipeline_descriptor.sample_mask,
            alpha_to_coverage_enabled: pipeline_descriptor.alpha_to_coverage_enabled,
        };

        device.create_render_pipeline(&mut render_pipeline_descriptor)
    }

    pub fn create_render_pass<'a>(
        &self,
        pass_descriptor: &PassDescriptor,
        encoder: &'a mut wgpu::CommandEncoder,
        frame: &'a wgpu::SwapChainOutput,
    ) -> wgpu::RenderPass<'a> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &pass_descriptor
                .color_attachments
                .iter()
                .map(|c| self.create_wgpu_color_attachment_descriptor(c, frame))
                .collect::<Vec<wgpu::RenderPassColorAttachmentDescriptor>>(),
            depth_stencil_attachment: pass_descriptor
                .depth_stencil_attachment
                .as_ref()
                .map(|d| self.create_wgpu_depth_stencil_attachment_descriptor(d, frame)),
        })
    }

    fn create_wgpu_color_attachment_descriptor<'a>(
        &'a self,
        color_attachment_descriptor: &RenderPassColorAttachmentDescriptor,
        frame: &'a wgpu::SwapChainOutput,
    ) -> wgpu::RenderPassColorAttachmentDescriptor<'a> {
        let attachment = match color_attachment_descriptor.attachment.as_str() {
            resource_name::texture::SWAP_CHAIN => &frame.view,
            _ => {
                match self
                    .render_resources
                    .get_named_resource(&color_attachment_descriptor.attachment)
                {
                    Some(resource) => self.textures.get(&resource).unwrap(),
                    None => panic!(
                        "Color attachment {} does not exist",
                        &color_attachment_descriptor.attachment
                    ),
                }
            }
        };

        let resolve_target = match color_attachment_descriptor.resolve_target {
            Some(ref target) => match target.as_str() {
                resource_name::texture::SWAP_CHAIN => Some(&frame.view),
                _ => match self.render_resources.get_named_resource(target.as_str()) {
                    Some(resource) => Some(self.textures.get(&resource).unwrap()),
                    None => panic!(
                        "Color attachment {} does not exist",
                        &color_attachment_descriptor.attachment
                    ),
                },
            },
            None => None,
        };

        wgpu::RenderPassColorAttachmentDescriptor {
            store_op: color_attachment_descriptor.store_op,
            load_op: color_attachment_descriptor.load_op,
            clear_color: color_attachment_descriptor.clear_color,
            attachment,
            resolve_target,
        }
    }

    fn create_wgpu_depth_stencil_attachment_descriptor<'a>(
        &'a self,
        depth_stencil_attachment_descriptor: &RenderPassDepthStencilAttachmentDescriptor,
        frame: &'a wgpu::SwapChainOutput,
    ) -> wgpu::RenderPassDepthStencilAttachmentDescriptor<&'a wgpu::TextureView> {
        let attachment = match depth_stencil_attachment_descriptor.attachment.as_str() {
            resource_name::texture::SWAP_CHAIN => &frame.view,
            _ => {
                match self
                    .render_resources
                    .get_named_resource(&depth_stencil_attachment_descriptor.attachment)
                {
                    Some(ref resource) => self.textures.get(&resource).unwrap(),
                    None => panic!(
                        "Depth stencil attachment {} does not exist",
                        &depth_stencil_attachment_descriptor.attachment
                    ),
                }
            }
        };

        wgpu::RenderPassDepthStencilAttachmentDescriptor {
            attachment,
            clear_depth: depth_stencil_attachment_descriptor.clear_depth,
            clear_stencil: depth_stencil_attachment_descriptor.clear_stencil,
            depth_load_op: depth_stencil_attachment_descriptor.depth_load_op,
            depth_store_op: depth_stencil_attachment_descriptor.depth_store_op,
            stencil_load_op: depth_stencil_attachment_descriptor.stencil_load_op,
            stencil_store_op: depth_stencil_attachment_descriptor.stencil_store_op,
        }
    }

    fn add_resource_info(&mut self, resource: RenderResource, resource_info: ResourceInfo) {
        self.resource_info.insert(resource, resource_info);
    }

    // TODO: consider moving this to a resource provider
    fn setup_bind_group(&mut self, bind_group: &BindGroup) -> u64 {
        let bind_group_id = bind_group.get_hash().unwrap();

        if let None = self.bind_groups.get(&bind_group_id) {
            let mut unset_uniforms = Vec::new();

            let mut binding_resources = Vec::with_capacity(bind_group.bindings.len());
            // if a uniform resource buffer doesn't exist, create a new empty one
            for binding in bind_group.bindings.iter() {
                let resource = match self.render_resources.get_named_resource(&binding.name) {
                    Some(resource) => resource,
                    None => {
                        println!(
                            "Warning: creating new empty buffer for binding {} {:?}",
                            binding.name, binding
                        );
                        unset_uniforms.push(binding.name.to_string());
                        match binding.bind_type {
                            BindType::Uniform { .. } => {
                                let size = binding.bind_type.get_uniform_size().unwrap();
                                let resource = self.create_buffer(
                                    size,
                                    wgpu::BufferUsage::UNIFORM | wgpu::BufferUsage::COPY_DST,
                                );

                                self.render_resources
                                    .set_named_resource(&binding.name, resource);
                                resource
                            }
                            _ => panic!("unsupported bind type: {:?}", binding),
                        }
                    }
                };

                binding_resources.push(resource);
            }

            // create wgpu Bindings
            let bindings = bind_group
                .bindings
                .iter()
                .zip(binding_resources)
                .map(|(binding, resource)| {
                    let resource_info = self.resource_info.get(&resource).unwrap();
                    wgpu::Binding {
                        binding: binding.index,
                        resource: match &binding.bind_type {
                            BindType::Uniform {
                                dynamic: _,
                                properties: _,
                            } => {
                                if let ResourceInfo::Buffer {
                                    size,
                                    buffer_usage: _,
                                } = resource_info
                                {
                                    let buffer = self.buffers.get(&resource).unwrap();
                                    wgpu::BindingResource::Buffer {
                                        buffer,
                                        range: 0..*size,
                                    }
                                } else {
                                    panic!("expected a Buffer resource");
                                }
                            }
                            _ => panic!("unsupported bind type"),
                        },
                    }
                })
                .collect::<Vec<wgpu::Binding>>();

            let bind_group_layout = self.bind_group_layouts.get(&bind_group_id).unwrap();
            let bind_group_descriptor = wgpu::BindGroupDescriptor {
                layout: bind_group_layout,
                bindings: bindings.as_slice(),
            };

            let bind_group = self.device.create_bind_group(&bind_group_descriptor);
            self.bind_groups.insert(
                bind_group_id,
                BindGroupInfo {
                    bind_group,
                    unset_uniforms,
                },
            );
        }

        bind_group_id
    }

    pub fn create_shader_module(
        device: &wgpu::Device,
        shader: &Shader,
        macros: Option<&[String]>,
    ) -> wgpu::ShaderModule {
        device.create_shader_module(&shader.get_spirv(macros))
    }

    pub fn initialize_resource_providers(
        &mut self,
        world: &mut World,
        render_graph: &mut RenderGraph,
    ) {
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { todo: 0 }),
        );
        for resource_provider in render_graph.resource_providers.iter_mut() {
            resource_provider.initialize(self, world);
        }

        // consume current encoder
        let command_buffer = self.encoder.take().unwrap().finish();
        self.queue.submit(&[command_buffer]);
    }
}

impl Renderer for WgpuRenderer {
    fn initialize(&mut self, world: &mut World, render_graph: &mut RenderGraph) {
        let (surface, window_size) = {
            let window = world.resources.get::<winit::window::Window>().unwrap();
            let surface = wgpu::Surface::create(window.deref());
            let window_size = window.inner_size();
            (surface, window_size)
        };

        self.surface = Some(surface);

        self.initialize_resource_providers(world, render_graph);

        self.resize(world, render_graph, window_size.width, window_size.height);
    }

    fn resize(
        &mut self,
        world: &mut World,
        render_graph: &mut RenderGraph,
        width: u32,
        height: u32,
    ) {
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { todo: 0 }),
        );
        self.swap_chain_descriptor.width = width;
        self.swap_chain_descriptor.height = height;
        let swap_chain = self
            .device
            .create_swap_chain(self.surface.as_ref().unwrap(), &self.swap_chain_descriptor);

        // WgpuRenderer can't own swap_chain without creating lifetime ergonomics issues, so lets just store it in World.
        world.resources.insert(swap_chain);
        for resource_provider in render_graph.resource_providers.iter_mut() {
            resource_provider.resize(self, world, width, height);
        }

        // consume current encoder
        let command_buffer = self.encoder.take().unwrap().finish();
        self.queue.submit(&[command_buffer]);
    }

    fn process_render_graph(&mut self, render_graph: &mut RenderGraph, world: &mut World) {
        // TODO: this self.encoder handoff is a bit gross, but its here to give resource providers access to buffer copies without
        // exposing the wgpu renderer internals to ResourceProvider traits. if this can be made cleaner that would be pretty cool.
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { todo: 0 }),
        );

        for resource_provider in render_graph.resource_providers.iter_mut() {
            resource_provider.update(self, world);
        }

        update_shader_assignments(world, render_graph);

        for (name, texture_descriptor) in render_graph.queued_textures.drain(..) {
            let resource = self.create_texture(&texture_descriptor);
            self.render_resources.set_named_resource(&name, resource);
        }

        let mut encoder = self.encoder.take().unwrap();

        let mut swap_chain = world.resources.get_mut::<wgpu::SwapChain>().unwrap();
        let frame = swap_chain
            .get_next_texture()
            .expect("Timeout when acquiring next swap chain texture");

        // self.setup_dynamic_entity_shader_uniforms(world, render_graph, &mut encoder);

        // setup, pipelines, bind groups, and resources
        let mut pipeline_storage = world
            .resources
            .get_mut::<AssetStorage<PipelineDescriptor>>()
            .unwrap();
        let shader_storage = world.resources.get::<AssetStorage<Shader>>().unwrap();

        for pipeline_descriptor_handle in render_graph.pipeline_descriptors.iter() {
            let pipeline_descriptor = pipeline_storage
                .get_mut(pipeline_descriptor_handle)
                .unwrap();
            // create pipelines
            if !self
                .render_pipelines
                .contains_key(pipeline_descriptor_handle)
            {
                let vertex_shader = shader_storage
                    .get(&pipeline_descriptor.shader_stages.vertex)
                    .unwrap();
                let fragment_shader = pipeline_descriptor
                    .shader_stages
                    .fragment
                    .as_ref()
                    .map(|handle| &*shader_storage.get(&handle).unwrap());
                let render_pipeline = WgpuRenderer::create_render_pipeline(
                    &self.render_resources,
                    &self.dynamic_uniform_buffer_info,
                    pipeline_descriptor,
                    &mut self.bind_group_layouts,
                    &self.device,
                    vertex_shader,
                    fragment_shader,
                );
                self.render_pipelines
                    .insert(pipeline_descriptor_handle.clone(), render_pipeline);
            }

            // create bind groups
            let pipeline_layout = pipeline_descriptor.get_layout().unwrap();
            for bind_group in pipeline_layout.bind_groups.iter() {
                self.setup_bind_group(bind_group);
            }
        }

        for (pass_name, pass_descriptor) in render_graph.pass_descriptors.iter() {
            // run passes
            let mut render_pass = self.create_render_pass(pass_descriptor, &mut encoder, &frame);
            if let Some(pass_pipelines) = render_graph.pass_pipelines.get(pass_name) {
                for pass_pipeline in pass_pipelines.iter() {
                    let pipeline_descriptor = pipeline_storage.get(pass_pipeline).unwrap();
                    let render_pipeline = self.render_pipelines.get(pass_pipeline).unwrap();
                    render_pass.set_pipeline(render_pipeline);

                    let mut render_pass = WgpuRenderPass {
                        render_pass: &mut render_pass,
                        renderer: self,
                        pipeline_descriptor,
                    };

                    for draw_target_name in pipeline_descriptor.draw_targets.iter() {
                        let draw_target = render_graph.draw_targets.get(draw_target_name).unwrap();
                        draw_target(world, &mut render_pass, pass_pipeline.clone());
                    }
                }
            }
        }

        let command_buffer = encoder.finish();
        self.queue.submit(&[command_buffer]);
    }

    fn create_buffer_with_data(
        &mut self,
        data: &[u8],
        buffer_usage: wgpu::BufferUsage,
    ) -> RenderResource {
        let resource = self.render_resources.get_next_resource();
        let buffer = self.device.create_buffer_with_data(data, buffer_usage);
        self.add_resource_info(
            resource,
            ResourceInfo::Buffer {
                buffer_usage,
                size: data.len() as u64,
            },
        );

        self.buffers.insert(resource, buffer);
        resource
    }

    fn create_buffer(&mut self, size: u64, buffer_usage: wgpu::BufferUsage) -> RenderResource {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            size,
            usage: buffer_usage,
        });

        let resource = self.render_resources.get_next_resource();
        self.add_resource_info(resource, ResourceInfo::Buffer { buffer_usage, size });

        self.buffers.insert(resource, buffer);
        resource
    }

    fn create_instance_buffer(
        &mut self,
        mesh_id: usize,
        size: usize,
        count: usize,
        buffer_usage: wgpu::BufferUsage,
    ) -> RenderResource {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            size: (size * count) as u64,
            usage: buffer_usage,
        });

        let resource = self.render_resources.get_next_resource();
        self.add_resource_info(
            resource,
            ResourceInfo::InstanceBuffer {
                buffer_usage,
                size,
                count,
                mesh_id,
            },
        );

        self.buffers.insert(resource, buffer);
        resource
    }

    fn create_instance_buffer_with_data(
        &mut self,
        mesh_id: usize,
        data: &[u8],
        size: usize,
        count: usize,
        buffer_usage: wgpu::BufferUsage,
    ) -> RenderResource {
        let buffer = self.device.create_buffer_with_data(data, buffer_usage);
        let resource = self.render_resources.get_next_resource();

        self.add_resource_info(
            resource,
            ResourceInfo::InstanceBuffer {
                buffer_usage,
                size,
                count,
                mesh_id,
            },
        );

        self.buffers.insert(resource, buffer);
        resource
    }

    fn get_resource_info(&self, resource: RenderResource) -> Option<&ResourceInfo> {
        self.resource_info.get(&resource)
    }

    fn remove_buffer(&mut self, resource: RenderResource) {
        self.buffers.remove(&resource);
        self.resource_info.remove(&resource);
    }

    fn create_buffer_mapped(
        &mut self,
        size: usize,
        buffer_usage: wgpu::BufferUsage,
        setup_data: &mut dyn FnMut(&mut [u8]),
    ) -> RenderResource {
        let mut mapped = self.device.create_buffer_mapped(size, buffer_usage);
        setup_data(&mut mapped.data);
        let buffer = mapped.finish();

        let resource = self.render_resources.get_next_resource();
        self.add_resource_info(
            resource,
            ResourceInfo::Buffer {
                buffer_usage,
                size: size as u64,
            },
        );

        self.buffers.insert(resource, buffer);
        resource
    }

    fn copy_buffer_to_buffer(
        &mut self,
        source_buffer: RenderResource,
        source_offset: u64,
        destination_buffer: RenderResource,
        destination_offset: u64,
        size: u64,
    ) {
        let source = self.buffers.get(&source_buffer).unwrap();
        let destination = self.buffers.get(&destination_buffer).unwrap();
        let encoder = self.encoder.as_mut().unwrap();
        encoder.copy_buffer_to_buffer(source, source_offset, destination, destination_offset, size);
    }
    fn get_dynamic_uniform_buffer_info(
        &self,
        resource: RenderResource,
    ) -> Option<&DynamicUniformBufferInfo> {
        self.dynamic_uniform_buffer_info.get(&resource)
    }

    fn get_dynamic_uniform_buffer_info_mut(
        &mut self,
        resource: RenderResource,
    ) -> Option<&mut DynamicUniformBufferInfo> {
        self.dynamic_uniform_buffer_info.get_mut(&resource)
    }

    fn add_dynamic_uniform_buffer_info(
        &mut self,
        resource: RenderResource,
        info: DynamicUniformBufferInfo,
    ) {
        self.dynamic_uniform_buffer_info.insert(resource, info);
    }

    fn create_texture(&mut self, texture_descriptor: &TextureDescriptor) -> RenderResource {
        let descriptor: wgpu::TextureDescriptor = (*texture_descriptor).into();
        let texture = self.device.create_texture(&descriptor);
        let resource = self.render_resources.get_next_resource();
        self.textures
            .insert(resource, texture.create_default_view());
        resource
    }

    fn create_texture_with_data(
        &mut self,
        texture_descriptor: &TextureDescriptor,
        bytes: Option<&[u8]>,
    ) -> RenderResource {
        let descriptor: wgpu::TextureDescriptor = (*texture_descriptor).into();
        let texture = self.device.create_texture(&descriptor);
        let texture_view = texture.create_default_view();
        if let Some(bytes) = bytes {
            let temp_buf = self
                .device
                .create_buffer_with_data(bytes, wgpu::BufferUsage::COPY_SRC);
            self.encoder.as_mut().unwrap().copy_buffer_to_texture(
                wgpu::BufferCopyView {
                    buffer: &temp_buf,
                    offset: 0,
                    row_pitch: 4 * descriptor.size.width,
                    image_height: descriptor.size.height,
                },
                wgpu::TextureCopyView {
                    texture: &texture,
                    mip_level: 0,
                    array_layer: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                },
                descriptor.size,
            );
        }

        let resource = self.render_resources.get_next_resource();
        self.add_resource_info(resource, ResourceInfo::Texture);
        resource
    }

    fn get_named_resource(&self, name: &str) -> Option<RenderResource> {
        self.render_resources.get_named_resource(name)
    }

    fn set_named_resource(&mut self, name: &str, resource: RenderResource) {
        self.render_resources.set_named_resource(name, resource);
    }

    fn remove_texture(&mut self, resource: RenderResource) {
        self.textures.remove(&resource);
        self.resource_info.remove(&resource);
    }
}

pub struct WgpuRenderPass<'a, 'b, 'c, 'd> {
    pub render_pass: &'b mut wgpu::RenderPass<'a>,
    pub pipeline_descriptor: &'c PipelineDescriptor,
    pub renderer: &'d mut WgpuRenderer,
}

impl<'a, 'b, 'c, 'd> RenderPass for WgpuRenderPass<'a, 'b, 'c, 'd> {
    fn get_renderer(&mut self) -> &mut dyn Renderer {
        self.renderer
    }

    fn get_pipeline_descriptor(&self) -> &PipelineDescriptor {
        self.pipeline_descriptor
    }

    fn set_vertex_buffer(&mut self, start_slot: u32, resource: RenderResource, offset: u64) {
        let buffer = self.renderer.buffers.get(&resource).unwrap();
        self.render_pass
            .set_vertex_buffers(start_slot, &[(&buffer, offset)]);
    }

    fn set_index_buffer(&mut self, resource: RenderResource, offset: u64) {
        let buffer = self.renderer.buffers.get(&resource).unwrap();
        self.render_pass.set_index_buffer(&buffer, offset);
    }

    fn draw_indexed(
        &mut self,
        indices: core::ops::Range<u32>,
        base_vertex: i32,
        instances: core::ops::Range<u32>,
    ) {
        self.render_pass
            .draw_indexed(indices, base_vertex, instances);
    }

    fn setup_bind_groups(&mut self, entity: Option<&Entity>) {
        let pipeline_layout = self.pipeline_descriptor.get_layout().unwrap();
        for bind_group in pipeline_layout.bind_groups.iter() {
            let bind_group_id = bind_group.get_hash().unwrap();
            let bind_group_info = self.renderer.bind_groups.get(&bind_group_id).unwrap();

            let mut dynamic_uniform_indices = Vec::new();
            for binding in bind_group.bindings.iter() {
                if let BindType::Uniform { dynamic, .. } = binding.bind_type {
                    if !dynamic {
                        continue;
                    }

                    if let Some(resource) = self
                        .renderer
                        .render_resources
                        .get_named_resource(&binding.name)
                    {
                        // PERF: This hashmap get is pretty expensive (10 fps for 10000 entities)
                        if let Some(dynamic_uniform_buffer_info) =
                            self.renderer.dynamic_uniform_buffer_info.get(&resource)
                        {
                            let index = dynamic_uniform_buffer_info
                                .offsets
                                .get(entity.unwrap())
                                .unwrap();

                            dynamic_uniform_indices.push(*index);
                        }
                    }
                }
            }

            // TODO: check to see if bind group is already set
            self.render_pass.set_bind_group(
                bind_group.index,
                &bind_group_info.bind_group,
                dynamic_uniform_indices.as_slice(),
            );
        }
    }
}

impl From<&BindType> for wgpu::BindingType {
    fn from(bind_type: &BindType) -> Self {
        match bind_type {
            BindType::Uniform {
                dynamic,
                properties: _,
            } => wgpu::BindingType::UniformBuffer { dynamic: *dynamic },
            BindType::Buffer { dynamic, readonly } => wgpu::BindingType::StorageBuffer {
                dynamic: *dynamic,
                readonly: *readonly,
            },
            BindType::SampledTexture {
                dimension,
                multisampled,
            } => wgpu::BindingType::SampledTexture {
                dimension: (*dimension).into(),
                multisampled: *multisampled,
            },
            BindType::Sampler => wgpu::BindingType::Sampler,
            BindType::StorageTexture { dimension } => wgpu::BindingType::StorageTexture {
                dimension: (*dimension).into(),
            },
        }
    }
}

pub struct BindGroupInfo {
    pub bind_group: wgpu::BindGroup,
    pub unset_uniforms: Vec<String>,
}
