# Emacs

Install [`m2-ls` with Cargo](cargo.md), then use Eglot, included with Emacs 29
and newer, to connect it to Macaulay2 buffers.

For Macaulay2's `M2-mode`, add this to your Emacs configuration:

    (with-eval-after-load 'eglot
      (add-to-list 'eglot-server-programs
                   '(M2-mode . ("m2-ls"))))

    (add-hook 'M2-mode-hook #'eglot-ensure)

This assumes Macaulay2's Emacs mode is already installed. If you use a
different major mode for `.m2` files, use its mode symbol and hook instead.

Open an `.m2` file and run `M-x eglot` if the connection has not started
already. Eglot exposes navigation through `xref`, completion through
`completion-at-point`, and formatting through `M-x eglot-format-buffer`.

This recipe has not been tested in Emacs. For client configuration details,
see the [Eglot manual](https://www.gnu.org/software/emacs/manual/html_node/eglot/).
