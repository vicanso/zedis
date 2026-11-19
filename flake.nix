{
    description = "Zedis: A blazing-fast, native Redis GUI built with Rust and GPUI.";

    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
        flake-utils.url = "github:numtide/flake-utils";
    };

    outputs = { self, nixpkgs, flake-utils }:
        flake-utils.lib.eachDefaultSystem(system:
            let
                pkgs = import nixpkgs { inherit system; };

                runtimeDeps = with pkgs; [
                    libxcb
                    libx11
                    libxcursor
                    libxrandr
                    libxi
                    libxkbcommon
                    wayland
                    vulkan-loader
                    libGL
                    fontconfig
                    freetype
                ];
            in {
                packages.default = pkgs.rustPlatform.buildRustPackage {
                    pname = "zedis";
                    version = "0.15.0";

                    src = ./.;
                    cargoLock.lockFile = ./Cargo.lock;

                    nativeBuildInputs = with pkgs; [
                        pkg-config
                        makeWrapper
                    ];
                    buildInputs = runtimeDeps;
                    LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeDeps;

                    postInstall = ''
                        install -Dm644 assets/zedis.desktop \
                            "$out/share/applications/zedis.desktop"

                        install -Dm644 assets/icon.png \
                            "$out/share/icons/hicolor/256x256/apps/zedis.png"

                        substituteInPlace "$out/share/applications/zedis.desktop" \
                            --replace-fail "Exec=zedis %u" "Exec=$out/bin/zedis %u"

                        wrapProgram "$out/bin/zedis" \
                            --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath runtimeDeps}"
                    '';


                    meta = with pkgs.lib; {
                        description = "A blazing-fast, native Redis GUI built with Rust and GPUI";
                        homepage = "https://github.com/vicanso/zedis";
                        license = licenses.asl20;
                        maintainers = [ ];
                        platforms = platforms.linux;
                        mainProgram = "zedis";
                    };
                };
            });
}
