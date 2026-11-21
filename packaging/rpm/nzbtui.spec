Name:           nzbtui
Version:        0.4.1
Release:        1%{?dist}
Summary:        Standalone Rust TUI Usenet client built with Ratatui

License:        GPL-3.0-or-later
URL:            https://github.com/soltros/nzbtui
Source0:        https://github.com/soltros/nzbtui/archive/refs/tags/v%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rustc
Recommends:     par2cmdline
Recommends:     p7zip
Recommends:     unrar
Recommends:     unzip

%description
Standalone terminal Usenet client without any web server or daemon.
Supports direct Newznab indexer search with season/episode filtering,
multiple NNTP/TLS providers with priority fallback, yEnc decoding with
CRC verification, on-demand PAR2 verification and repair, automatic
archive extraction, and tagged download export.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --locked

%install
install -Dm755 target/release/nzbtui %{buildroot}%{_bindir}/nzbtui
install -Dm644 debian/nzbtui.1 %{buildroot}%{_mandir}/man1/nzbtui.1
install -Dm644 README.md %{buildroot}%{_docdir}/%{name}/README.md
install -Dm644 LICENSE %{buildroot}%{_licensedir}/%{name}/LICENSE

%files
%license LICENSE
%doc README.md
%{_bindir}/nzbtui
%{_mandir}/man1/nzbtui.1*

%changelog
* Fri Nov 20 2026 Derrik Diener <142091676+soltros@users.noreply.github.com> - 0.3.1-1
- Release v0.3.1 with Download Export, Season/Episode filters, Cleanup, and Self-Updater
