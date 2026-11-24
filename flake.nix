{
  description = "DMM mod manager development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      fenix,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            pkgs.libxkbcommon
            pkgs.libxcb
            pkgs.wayland
          ];
          packages = [
            pkgs.stdenv.cc.cc.lib
            pkgs.tracy
            fenix.packages.${system}.complete.toolchain
            pkgs.alsa-lib
            pkgs.fontconfig
            pkgs.freetype
            pkgs.glib
            pkgs.gst_all_1.gstreamer
            pkgs.gst_all_1.gst-plugins-base
            pkgs.gst_all_1.gst-plugins-good
            pkgs.gst_all_1.gst-plugins-bad
            pkgs.glib-networking
            pkgs.pkg-config
            pkgs.vulkan-loader
          ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            pkgs.alsa-lib
            pkgs.fontconfig
            pkgs.freetype
            pkgs.glib
            pkgs.gst_all_1.gstreamer
            pkgs.gst_all_1.gst-plugins-base
            pkgs.gst_all_1.gst-plugins-good
            pkgs.gst_all_1.gst-plugins-bad
            pkgs.glib-networking
            pkgs.libxkbcommon
            pkgs.libxcb
            pkgs.stdenv.cc.cc.lib
            pkgs.wayland
            pkgs.vulkan-loader
          ];
          GST_PLUGIN_SYSTEM_PATH_1_0 = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" [
            pkgs.gst_all_1.gst-plugins-base
            pkgs.gst_all_1.gst-plugins-good
            pkgs.gst_all_1.gst-plugins-bad
          ];
          # libsoup (GStreamer's https source) gets its TLS backend from
          # glib-networking; without it every remote https video fails with
          # "TLS support is not available".
          GIO_EXTRA_MODULES = "${pkgs.glib-networking}/lib/gio/modules";
        };
      }
    );
}
