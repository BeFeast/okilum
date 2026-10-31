# Static native decoder experiment configuration (before measurements)

The immutable parent GATE.md thresholds are unchanged.

Two explicit source-build configurations are registered before decoding:

1. `stock`: exact upstream libheif1.22.2 + libde2651.0.16, default decode filters.
2. `csp-profile`: same pinned sources, a documented one-line compile-profile patch to decoder_libde265.cc makes its existing __EMSCRIPTEN__ deblocking/SAO-disabled branch selectable by OKILUM_HEIC_CSP_FILTER_PROFILE. This matches the actual heic-to/csp profile, not a quality-tolerance change. Driver image selection is first top-level image ID; libheif default RGBA8 decode transform/color settings match its Emscripten wrapper calling heif_decode_image(...,nullptr).

Both builds are static codec archives under an isolated QA prefix, with every encoder/other decoder/plugin/dlopen/sharpyuv feature disabled. Only standard system C/C++ runtime libraries are permitted dynamically.

Set context max_image_size_pixels=64,000,000 BEFORE reading/parsing/decoding and check first handle dimensions before output allocation. Source cap50MiB before read. Context codec-config SPS geometry is checked by upstream decoder.cc before plugin decode; source has a TODO for replacement SPS inside the image bitstream, so the experiment must not claim universal coded-plane preallocation proof without resolving/testing that path.

No native GUI, product integration, source push, package install or library adoption is part of this experiment.

Setup correction before any native image decode: upstream unconditionally registers a built-in mask encoder even with all CMake encoder optionsOFF. Both profiles carry a common explicit decode-only registration patch omitting it; stock means stock decode-filter behavior, not byte-unmodified plugin registration. External helper image-I/O package discovery is also disabled through CMAKE_DISABLE_FIND_PACKAGE_{TIFF,JPEG,PNG,WEBP,ZLIB,Brotli}; it is never linked into the driver. Runtime closure must assert exactly1de265decoder/0encoders.
