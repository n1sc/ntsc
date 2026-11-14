;;; neutron-mode.el --- Neutron editing with Eglot -*- lexical-binding: t; -*-

;; Copyright (c) 2026 yasakei
;; Version: 0.1.0
;; Package-Requires: ((emacs "29.1"))
;; URL: https://github.com/n1sc/ntsc
;; Keywords: languages
;; License: NPL-1.1 (see LICENSE)

;;; Commentary:
;; Native Eglot provides language intelligence through ntsc lsp.
;; Compilation commands are explicit and run at the nearest neutron.toml.

;;; Code:

(require 'cl-lib)
(require 'compile)
(require 'eglot)
(require 'project)
(require 'neutron-data)

(defgroup neutron nil "Neutron language support." :group 'languages)

(defcustom neutron-server-command neutron--server-command
  "Executable and arguments for the Neutron language server."
  :type '(repeat string)
  :group 'neutron)

(defcustom neutron-cli-command neutron--cli
  "Compiler executable for explicit project workflows."
  :type 'string
  :group 'neutron)

(defcustom neutron-enable-eglot t
  "Whether Neutron buffers should start the Eglot client."
  :type 'boolean
  :group 'neutron)

(defun neutron--project-root ()
  "Return the nearest Neutron project root, or nil."
  (locate-dominating-file (or buffer-file-name default-directory)
                          neutron--project-marker))

(defun neutron--project-find (directory)
  "Recognize a Neutron project above DIRECTORY."
  (when-let* ((root (locate-dominating-file directory neutron--project-marker)))
    (cons 'neutron root)))

(cl-defmethod project-root ((project (head neutron)))
  (cdr project))

(add-hook 'project-find-functions #'neutron--project-find)

(defun neutron--eglot-contact (&optional _interactive)
  "Return the configured server command."
  neutron-server-command)

(add-to-list 'eglot-server-programs '(neutron-mode . neutron--eglot-contact))

(defun neutron--workflow (action)
  "Save project buffers and execute compiler workflow ACTION."
  (let* ((root (or (neutron--project-root)
                   (user-error "Open a project containing %s" neutron--project-marker)))
         (arguments (alist-get action neutron--workflows))
         (default-directory root))
    (unless arguments
      (user-error "Unknown Neutron workflow: %s" action))
    (save-some-buffers nil
                       (lambda ()
                         (and buffer-file-name
                              (file-in-directory-p buffer-file-name root))))
    (when (cl-some (lambda (buffer)
                     (with-current-buffer buffer
                       (and buffer-file-name
                            (file-in-directory-p buffer-file-name root)
                            (buffer-modified-p))))
                   (buffer-list))
      (user-error "Save modified project buffers before running the compiler"))
    (compile (mapconcat #'shell-quote-argument
                        (cons neutron-cli-command arguments) " "))))

(defun neutron-build ()
  "Build the current Neutron project."
  (interactive)
  (neutron--workflow 'build))

(defun neutron-run ()
  "Build and run the current Neutron project."
  (interactive)
  (neutron--workflow 'run))

(defun neutron-test ()
  "Run all test blocks in the current Neutron project."
  (interactive)
  (neutron--workflow 'test))

(defun neutron-format ()
  "Format the buffer using the shared language server."
  (interactive)
  (eglot-format-buffer))

(defun neutron-restart ()
  "Restart the current Neutron language server."
  (interactive)
  (eglot-reconnect (or (eglot-current-server)
                       (user-error "No Neutron language server is running"))))

(defvar neutron-mode-syntax-table
  (let ((table (make-syntax-table)))
    (modify-syntax-entry ?# "<" table)
    (modify-syntax-entry ?\n ">" table)
    (modify-syntax-entry ?\" "\"" table)
    (modify-syntax-entry ?\\ "\\" table)
    (modify-syntax-entry ?_ "w" table)
    (modify-syntax-entry ?{ "(}" table)
    (modify-syntax-entry ?} "){" table)
    table))

(defvar neutron-font-lock-keywords
  `((,(regexp-opt neutron--keywords 'symbols) . font-lock-keyword-face)
    (,(regexp-opt neutron--types 'symbols) . font-lock-type-face)
    (,(regexp-opt neutron--constants 'symbols) . font-lock-constant-face)
    ("\\_<\\(fun\\|class\\|enum\\|trait\\)\\_>\\s-+\\([[:word:]_]+\\)"
     2 font-lock-function-name-face)))

(defvar neutron-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-b") #'neutron-build)
    (define-key map (kbd "C-c C-r") #'neutron-run)
    (define-key map (kbd "C-c C-t") #'neutron-test)
    (define-key map (kbd "C-c C-f") #'neutron-format)
    map))

;;;###autoload
(define-derived-mode neutron-mode prog-mode "Neutron"
  "Edit Neutron source using the shared NTSC language server."
  :syntax-table neutron-mode-syntax-table
  (setq-local comment-start "# ")
  (setq-local comment-end "")
  (setq-local indent-tabs-mode nil)
  (setq-local tab-width 4)
  (setq-local font-lock-defaults '(neutron-font-lock-keywords))
  (when (and neutron-enable-eglot buffer-file-name)
    (eglot-ensure)))

;;;###autoload
(add-to-list 'auto-mode-alist '("\\.nt\\'" . neutron-mode))

(provide 'neutron-mode)
;;; neutron-mode.el ends here
