//! Compiles the real adjustment shader on the real GPU and binds a buffer
//! of exactly `size_of::<AllAdjustments>()` against the auto-derived
//! layout. This catches two whole classes of regression at test time
//! instead of at app launch: WGSL that fails naga validation, and Rust ↔
//! WGSL struct layout drift (the auto layout's min_binding_size is the
//! WGSL struct size — a too-small Rust struct fails bind group creation).

use rapidraw_lib::image_processing::AllAdjustments;

#[test]
fn shader_compiles_and_struct_layout_matches() {
    let instance = wgpu::Instance::default();
    let adapter =
        match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        {
            Ok(a) => a,
            Err(_) => {
                eprintln!("no GPU adapter available; skipping");
                return;
            }
        };
    let (device, _queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .expect("device");

    // Fail the test on validation errors instead of panicking asynchronously.
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("adjustments shader under test"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../src/shaders/shader.wgsl").into()),
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("test pipeline"),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });

    // Dummy resources matching every binding slot.
    let tex = |format: wgpu::TextureFormat, dim: wgpu::TextureDimension, layers: u32, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: dim,
            format,
            usage,
            view_formats: &[],
        })
    };
    use wgpu::TextureUsages as TU;
    let input = tex(
        wgpu::TextureFormat::Rgba32Float,
        wgpu::TextureDimension::D2,
        1,
        TU::TEXTURE_BINDING,
    );
    let output = tex(
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureDimension::D2,
        1,
        TU::STORAGE_BINDING,
    );
    let masks = tex(
        wgpu::TextureFormat::R8Unorm,
        wgpu::TextureDimension::D2,
        1,
        TU::TEXTURE_BINDING,
    );
    let lut = tex(
        wgpu::TextureFormat::Rgba32Float,
        wgpu::TextureDimension::D3,
        2,
        TU::TEXTURE_BINDING,
    );
    let blur = || {
        tex(
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureDimension::D2,
            1,
            TU::TEXTURE_BINDING,
        )
    };
    let (b1, b2, b3, b4) = (blur(), blur(), blur(), blur());
    // The flare texture is sampled with a filtering sampler, so it needs a
    // filterable format (the app uses rgba16float there).
    let flare = tex(
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureDimension::D2,
        1,
        TU::TEXTURE_BINDING,
    );
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());

    // THE layout probe: exactly the Rust struct's size.
    let adjustments_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("adjustments probe"),
        size: std::mem::size_of::<AllAdjustments>() as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });

    let view = |t: &wgpu::Texture, dim| {
        t.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(dim),
            ..Default::default()
        })
    };
    use wgpu::TextureViewDimension as TVD;
    let entries = [
        wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view(&input, TVD::D2)),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::TextureView(&view(&output, TVD::D2)),
        },
        wgpu::BindGroupEntry {
            binding: 2,
            resource: adjustments_buffer.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 3,
            resource: wgpu::BindingResource::TextureView(&view(&masks, TVD::D2Array)),
        },
        wgpu::BindGroupEntry {
            binding: 4,
            resource: wgpu::BindingResource::TextureView(&view(&lut, TVD::D3)),
        },
        // binding 5 (lut_sampler) is statically unused — the auto layout drops it.
        wgpu::BindGroupEntry {
            binding: 6,
            resource: wgpu::BindingResource::TextureView(&view(&b1, TVD::D2)),
        },
        wgpu::BindGroupEntry {
            binding: 7,
            resource: wgpu::BindingResource::TextureView(&view(&b2, TVD::D2)),
        },
        wgpu::BindGroupEntry {
            binding: 8,
            resource: wgpu::BindingResource::TextureView(&view(&b3, TVD::D2)),
        },
        wgpu::BindGroupEntry {
            binding: 9,
            resource: wgpu::BindingResource::TextureView(&view(&b4, TVD::D2)),
        },
        wgpu::BindGroupEntry {
            binding: 10,
            resource: wgpu::BindingResource::TextureView(&view(&flare, TVD::D2)),
        },
        wgpu::BindGroupEntry {
            binding: 11,
            resource: wgpu::BindingResource::Sampler(&sampler),
        },
    ];
    let _bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("layout probe"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &entries,
    });

    let err = pollster::block_on(error_scope.pop());
    assert!(
        err.is_none(),
        "shader/pipeline/layout validation failed: {:?}",
        err
    );

    // Negative probe: a buffer one alignment-step smaller must FAIL, which
    // pins the WGSL struct size to within 16 bytes of the Rust struct —
    // i.e. catches drift in either direction.
    let scope2 = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let small_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("undersized probe"),
        size: std::mem::size_of::<AllAdjustments>() as u64 - 16,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let mut small_entries = entries.clone();
    small_entries[2] = wgpu::BindGroupEntry {
        binding: 2,
        resource: small_buffer.as_entire_binding(),
    };
    let _bg2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("undersized layout probe"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &small_entries,
    });
    let err2 = pollster::block_on(scope2.pop());
    assert!(
        err2.is_some(),
        "an undersized buffer bound cleanly — the Rust struct is larger than the WGSL struct (layout drift)"
    );
}
