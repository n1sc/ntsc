class Ntsc < Formula
  desc "Neutron Type-Safe Compiler: a statically typed, memory-safe systems language compiled through LLVM"
  homepage "https://github.com/n1sc/ntsc"
  version "@VERSION@"
  # The Neutron Permissive License is not an SPDX identifier.
  license :cannot_represent
  depends_on :macos

  on_macos do
    on_arm do
      url "https://github.com/n1sc/ntsc/releases/download/v@VERSION@/ntsc-@VERSION@-macos-arm64.tar.gz"
      sha256 "@SHA256_MACOS_ARM@"
    end
    on_intel do
      url "https://github.com/n1sc/ntsc/releases/download/v@VERSION@/ntsc-@VERSION@-macos-x86_64.tar.gz"
      sha256 "@SHA256_MACOS_INTEL@"
    end
  end

  def install
    # The tarball is self-contained: bin/ holds the compiler, the package
    # manager and the language server; lib/ntsc/ holds the runtime archive and
    # the bundled LLVM dylibs. Installing the tree under libexec keeps that
    # layout intact, which matters because the binaries resolve their runtime
    # through @executable_path/../lib/ntsc.
    libexec.install "bin", "lib", "share"
    # Exec the real binaries instead of symlinking to them, so the shims in
    # bin/ keep pointing at a complete tree.
    bin.write_exec_script libexec/"bin/ntsc"
    bin.write_exec_script libexec/"bin/ntsc-pkg"
    bin.write_exec_script libexec/"bin/ntsc-lsp"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ntsc --version")
  end
end
