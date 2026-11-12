{ lib
, rustPlatform
, pkg-config
, openssl
, makeWrapper
, mpv
, yt-dlp
, ffmpeg
}:

rustPlatform.buildRustPackage {
  pname = "ytmtui";
  version = (lib.importTOML ../Cargo.toml).package.version;

  src = lib.cleanSourceWith {
    src = lib.cleanSource ../.;
    filter = path: _type:
      let
        base = baseNameOf (toString path);
      in
      !builtins.elem base [ "target" "result" ".direnv" ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkg-config makeWrapper ];
  # reqwest with default-tls (native-tls) links against OpenSSL.
  buildInputs = [ openssl ];

  # Runtime dependencies are wrapped into PATH, so playback and downloads
  # work regardless of the user's profile.
  postInstall = ''
    wrapProgram $out/bin/ytmtui \
      --prefix PATH : ${lib.makeBinPath [ mpv yt-dlp ffmpeg ]}
  '';

  meta = {
    description = "A full-featured lightning fast YouTube Music TUI client written in Rust";
    homepage = "https://github.com/sh1zicus/ytmtui";
    license = lib.licenses.mit;
    mainProgram = "ytmtui";
    platforms = lib.platforms.linux;
  };
}
