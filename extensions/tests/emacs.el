;;; emacs.el --- Neutron client tests -*- lexical-binding: t; -*-
(require 'ert)
(setq neutron-enable-eglot nil)
(require 'neutron-mode)

(ert-deftest neutron-client-language-basics ()
  (with-temp-buffer
    (neutron-mode)
    (should (eq major-mode 'neutron-mode))
    (should (equal comment-start "# "))
    (should (equal neutron-server-command '("ntsc" "lsp")))
    (should (equal (alist-get 'build neutron--workflows) '("build")))
    (insert "fun main() { say(\"# not a comment\"); } # comment")
    (font-lock-ensure)
    (goto-char (point-min))
    (search-forward "# not")
    (should (nth 3 (syntax-ppss)))
    (search-forward "# comment")
    (should (nth 4 (syntax-ppss)))))

(ert-deftest neutron-client-nearest-project ()
  (let* ((directory (make-temp-file "neutron editor " t))
         (nested (expand-file-name "nested/" directory)))
    (unwind-protect
        (progn
          (make-directory nested)
          (write-region "" nil (expand-file-name "neutron.toml" directory) nil 'silent)
          (write-region "" nil (expand-file-name "neutron.toml" nested) nil 'silent)
          (with-temp-buffer
            (setq buffer-file-name (expand-file-name "main.nt" nested))
            (should (equal (neutron--project-root) nested))
            (should (equal (project-root (neutron--project-find nested)) nested))))
      (delete-directory directory t))))

(ert-run-tests-batch-and-exit)
