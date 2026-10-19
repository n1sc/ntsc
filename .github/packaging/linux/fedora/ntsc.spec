Name:           ntsc
Version:        @VERSION@
Release:        1%{?dist}
Summary:        Neutron Type-Safe Compiler

License:        NPL-1.1
URL:            https://github.com/yasakei/neutron-rs
Requires:       llvm-libs >= 23

%description
A statically typed, memory-safe systems language that compiles to native
binaries through LLVM. Values move by default, borrows are declared with
view, and memory is released deterministically when ownership ends.

The ntsc binary links LLVM 23 dynamically; this package therefore requires
the llvm-libs runtime.

%install
install -Dm755 %{_sourcedir}/ntsc %{buildroot}%{_bindir}/ntsc
install -Dm755 %{_sourcedir}/ntsc-pkg %{buildroot}%{_bindir}/ntsc-pkg
install -Dm755 %{_sourcedir}/ntsc-lsp %{buildroot}%{_bindir}/ntsc-lsp
# The static archive `ntsc build` links every NTSC program against. %{_libdir}
# expands to /usr/lib64 on x86_64; an installed ntsc finds it there via its
# executable-relative <prefix>/lib64/ntsc lookup.
install -Dm644 %{_sourcedir}/libntsc_runtime.a %{buildroot}%{_libdir}/ntsc/libntsc_runtime.a
install -Dm644 %{_sourcedir}/ntsc.1.gz %{buildroot}%{_mandir}/man1/ntsc.1.gz

%files
%{_bindir}/ntsc
%{_bindir}/ntsc-pkg
%{_bindir}/ntsc-lsp
%{_libdir}/ntsc/libntsc_runtime.a
%{_mandir}/man1/ntsc.1.gz
