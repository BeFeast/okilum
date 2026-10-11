# Request correlation fixtures (#1166)

Synthetic: an ingress-nginx access log and the checkout app behind it, three
requests. They share each request's id under different keys (`request_id`
at the ingress, `x-request-id` in the app), as the #1166 acceptance asks;
the sample pack (#1155) has no id shared across files.
