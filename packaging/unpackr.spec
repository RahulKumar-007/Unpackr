Name:           unpackr
Version:        0.1.0
Release:        1%{?dist}
Summary:        Low-disk-space archive extraction engine and native GUI

License:        MIT OR Apache-2.0
URL:            https://github.com/RahulKumar-007/Unpackr
Source0:        https://github.com/RahulKumar-007/Unpackr/archive/refs/tags/v%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  desktop-file-utils
BuildRequires:  libxkbcommon-devel
BuildRequires:  wayland-devel
BuildRequires:  mesa-libGL-devel
BuildRequires:  fontconfig-devel

%description
Unpackr is a production-quality, low-disk-space archive extraction engine
and native desktop GUI. It eliminates the classic 2x disk capacity requirement
by progressively punching filesystem holes in already-extracted archive payload
blocks in-place while strictly preserving ZIP structure, CRC-32 integrity,
and crash resumability.

%prep
%autosetup -n Unpackr-%{version}

%build
cargo build --release

%install
rm -rf $RPM_BUILD_ROOT
install -d -m 0755 %{buildroot}%{_bindir}
install -m 0755 target/release/unpackr %{buildroot}%{_bindir}/unpackr

# Desktop entry & Icon
install -d -m 0755 %{buildroot}%{_datadir}/applications
install -m 0644 extra/unpackr.desktop %{buildroot}%{_datadir}/applications/unpackr.desktop

install -d -m 0755 %{buildroot}%{_datadir}/icons/hicolor/scalable/apps
install -m 0644 extra/unpackr.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/unpackr.svg

# Shell completions
install -d -m 0755 %{buildroot}%{_datadir}/bash-completion/completions
target/release/unpackr completions bash > %{buildroot}%{_datadir}/bash-completion/completions/unpackr

install -d -m 0755 %{buildroot}%{_datadir}/zsh/site-functions
target/release/unpackr completions zsh > %{buildroot}%{_datadir}/zsh/site-functions/_unpackr

install -d -m 0755 %{buildroot}%{_datadir}/fish/vendor_completions.d
target/release/unpackr completions fish > %{buildroot}%{_datadir}/fish/vendor_completions.d/unpackr.fish

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/unpackr.desktop

%files
%license LICENSE-MIT LICENSE-APACHE
%doc README.md
%{_bindir}/unpackr
%{_datadir}/applications/unpackr.desktop
%{_datadir}/icons/hicolor/scalable/apps/unpackr.svg
%{_datadir}/bash-completion/completions/unpackr
%{_datadir}/zsh/site-functions/_unpackr
%{_datadir}/fish/vendor_completions.d/unpackr.fish

%changelog
* Fri Sep 25 2026 Rahul Kumar <chhonkarrahul1362@gmail.com> - 0.1.0-1
- Initial release v0.1.0
