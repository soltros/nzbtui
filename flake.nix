{
  description = "Standalone Rust TUI Usenet client built with Ratatui";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            version = cargoToml.package.version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [ pkgs.installShellFiles ];

            postInstall = ''
              installManPage debian/nzbtui.1
              install -Dm644 README.md $out/share/doc/nzbtui/README.md
              install -Dm644 LICENSE $out/share/licenses/nzbtui/LICENSE
            '';

            meta = with pkgs.lib; {
              description = cargoToml.package.description;
              homepage = "https://github.com/soltros/nzbtui";
              license = licenses.gpl3Plus;
              mainProgram = "nzbtui";
              platforms = platforms.linux;
            };
          };
          nzbtui = self.packages.${system}.default;
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/nzbtui";
        };
      });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              cargo
              rustc
              rust-analyzer
              par2cmdline
              p7zip
              unzip
            ];
          };
        });
    };
}
