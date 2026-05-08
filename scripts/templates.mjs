import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import vendoredCatalog from '../corpus/catalog.json' with { type: 'json' };

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const REMOTE_TEMPLATES = [
  [
    'leemunroe-inlined',
    'https://raw.githubusercontent.com/leemunroe/responsive-html-email-template/master/email-inlined.html',
  ],
  [
    'mailgun-action',
    'https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/action.html',
  ],
  [
    'mailgun-alert',
    'https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/alert.html',
  ],
  [
    'mailgun-billing',
    'https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/billing.html',
  ],
  [
    'waypoint-banking-payout',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/banking-payout.html',
  ],
  [
    'waypoint-banking-refund-approved',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/banking-refund-approved.html',
  ],
  [
    'waypoint-banking-withdrawal',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/banking-withdrawal.html',
  ],
  [
    'waypoint-ecommerce-delivery',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-delivery-notification.html',
  ],
  [
    'waypoint-ecommerce-milestone-reward',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-milestone-reward.html',
  ],
  [
    'waypoint-ecommerce-new-order-receipt',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-new-order-receipt.html',
  ],
  [
    'waypoint-ecommerce-promo-code',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-promo-code.html',
  ],
  [
    'waypoint-ecommerce-reengagement',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-reengagement.html',
  ],
  [
    'waypoint-ecommerce-welcome',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-welcome.html',
  ],
  [
    'waypoint-marketplace-credit-expiring',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-credit-expiring.html',
  ],
  [
    'waypoint-marketplace-first-listing-notification',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-first-listing-notification.html',
  ],
  [
    'waypoint-marketplace-new-delivery',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-new-delivery.html',
  ],
  [
    'waypoint-marketplace-qr',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-qr-tickets.html',
  ],
  [
    'waypoint-marketplace-reservation-reminder',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-reservation-reminder.html',
  ],
  [
    'waypoint-marketplace-respond-to-inquiry',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-respond-to-inquiry.html',
  ],
  [
    'waypoint-marketplace-saved-search',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-saved-search.html',
  ],
  [
    'waypoint-saas-accept-invite',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-accept-invite.html',
  ],
  [
    'waypoint-saas-app-store-links',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-app-store-links.html',
  ],
  [
    'waypoint-saas-credit-usage-report',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-credit-usage-report.html',
  ],
  [
    'waypoint-saas-download-ready',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-download-ready.html',
  ],
  [
    'waypoint-saas-first-time-watched',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-first-time-watched.html',
  ],
  [
    'waypoint-saas-milestone-next-steps',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-milestone-next-steps.html',
  ],
  [
    'waypoint-saas-otp',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-one-time-passcode-otp.html',
  ],
  [
    'waypoint-saas-password-changed',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-password-changed.html',
  ],
  [
    'waypoint-saas-payment-declined',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-payment-declined.html',
  ],
  [
    'waypoint-saas-reset-password',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-reset-password.html',
  ],
  [
    'waypoint-saas-receipt',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-subscription-receipt.html',
  ],
  [
    'waypoint-saas-trial-ends-soon',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-trial-ends-soon.html',
  ],
  [
    'waypoint-saas-weekly-metrics-report',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-weekly-metrics-report.html',
  ],
  [
    'waypoint-saas-welcome',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-welcome.html',
  ],
  [
    'waypoint-social-new-comment',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/social-new-comment.html',
  ],
  [
    'waypoint-social-new-follower',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/social-new-follower.html',
  ],
  [
    'waypoint-social-post-metrics',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/social-post-metrics.html',
  ],
  [
    'waypoint-social-yearly-wrapped-report',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/social-yearly-wrapped-report.html',
  ],
  [
    'mailpace-welcome',
    'https://raw.githubusercontent.com/mailpace/templates/main/dist/welcome.html',
  ],
  [
    'mailpace-confirmation',
    'https://raw.githubusercontent.com/mailpace/templates/main/dist/confirmation.html',
  ],
  [
    'mailpace-password-reset',
    'https://raw.githubusercontent.com/mailpace/templates/main/dist/password_reset.html',
  ],
  [
    'mailpace-receipt',
    'https://raw.githubusercontent.com/mailpace/templates/main/dist/receipt.html',
  ],
  [
    'mailpace-security-alert',
    'https://raw.githubusercontent.com/mailpace/templates/main/dist/security_alert.html',
  ],
  [
    'mailpace-account-deleted',
    'https://raw.githubusercontent.com/mailpace/templates/main/dist/account_deleted.html',
  ],
  [
    'postmark-comment-notification',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/comment-notification/content.html',
  ],
  [
    'postmark-dunning',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/dunning/content.html',
  ],
  [
    'postmark-example',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/example/content.html',
  ],
  [
    'postmark-invoice',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/invoice/content.html',
  ],
  [
    'postmark-password-reset-help',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/password-reset-help/content.html',
  ],
  [
    'postmark-password-reset',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/password-reset/content.html',
  ],
  [
    'postmark-receipt',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/receipt/content.html',
  ],
  [
    'postmark-trial-expired',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/trial-expired/content.html',
  ],
  [
    'postmark-trial-expiring',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/trial-expiring/content.html',
  ],
  [
    'postmark-user-invitation',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/user-invitation/content.html',
  ],
  [
    'postmark-welcome',
    'https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/welcome/content.html',
  ],
  [
    'mailersend-password-reminder',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/01-%20Setting%20Up%20Account%20/password-reminder.html',
  ],
  [
    'mailersend-password-reminder-2',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/01-%20Setting%20Up%20Account%20/password-reminder-2.html',
  ],
  [
    'mailersend-welcome-01',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/01-%20Setting%20Up%20Account%20/welcome-email-01.html',
  ],
  [
    'mailersend-welcome-2',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/01-%20Setting%20Up%20Account%20/welcome-email-2.html',
  ],
  [
    'mailersend-invoice',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/02-%20Order%20Management%20/invoice.html',
  ],
  [
    'mailersend-invoice-2',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/02-%20Order%20Management%20/invoice-2.html',
  ],
  [
    'mailersend-order-confirmation',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/02-%20Order%20Management%20/order-confirmation.html',
  ],
  [
    'mailersend-feedback',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/03-%20Keeping%20in%20Touch%20/feedback.html',
  ],
  [
    'mailersend-product-announcement',
    'https://raw.githubusercontent.com/mailersend/transactional-email-templates/main/03-%20Keeping%20in%20Touch%20/product-announcement.html',
  ],
  [
    'sendgrid-dynamic-receipt',
    'https://raw.githubusercontent.com/sendgrid/email-templates/master/dynamic-templates/receipt/receipt.html',
  ],
  [
    'sendgrid-call-to-action',
    'https://raw.githubusercontent.com/sendgrid/email-templates/master/dynamic-templates/transactional-actions/call_to_action.html',
  ],
  [
    'sendgrid-paste-password-reset',
    'https://raw.githubusercontent.com/sendgrid/email-templates/master/paste-templates/password-reset.html',
  ],
  [
    'sendgrid-paste-welcome',
    'https://raw.githubusercontent.com/sendgrid/email-templates/master/paste-templates/welcome.html',
  ],
  [
    'sendgrid-merriweather-receipt',
    'https://raw.githubusercontent.com/sendgrid/email-templates/master/merriweather-templates/receipt.html',
  ],
  [
    'sendgrid-merriweather-welcome',
    'https://raw.githubusercontent.com/sendgrid/email-templates/master/merriweather-templates/welcome.html',
  ],
  [
    'ckissi-welcome',
    'https://raw.githubusercontent.com/ckissi/responsive-html-email-templates/master/Welcome/welcome.html',
  ],
  [
    'ckissi-reset-password',
    'https://raw.githubusercontent.com/ckissi/responsive-html-email-templates/master/Reset%20password/reset-password.html',
  ],
  [
    'ckissi-confirm-email',
    'https://raw.githubusercontent.com/ckissi/responsive-html-email-templates/master/Confirm%20Email/confirm-email.html',
  ],
  [
    'ckissi-invoice',
    'https://raw.githubusercontent.com/ckissi/responsive-html-email-templates/master/Invoice/invoice.html',
  ],
  [
    'ckissi-trial-expired',
    'https://raw.githubusercontent.com/ckissi/responsive-html-email-templates/master/Trial%20Expired/trial-expired.html',
  ],
  [
    'davidamunga-welcome',
    'https://raw.githubusercontent.com/DavidAmunga/html-email-templates/master/welcome.html',
  ],
  [
    'davidamunga-reset-password',
    'https://raw.githubusercontent.com/DavidAmunga/html-email-templates/master/reset-password.html',
  ],
  [
    'colorlib-template-1',
    'https://raw.githubusercontent.com/ColorlibHQ/email-templates/master/1/index.html',
  ],
  [
    'colorlib-template-2',
    'https://raw.githubusercontent.com/ColorlibHQ/email-templates/master/2/index.html',
  ],
  [
    'colorlib-template-3',
    'https://raw.githubusercontent.com/ColorlibHQ/email-templates/master/3/index.html',
  ],
  [
    'colorlib-template-4',
    'https://raw.githubusercontent.com/ColorlibHQ/email-templates/master/4/index.html',
  ],
  [
    'colorlib-template-5',
    'https://raw.githubusercontent.com/ColorlibHQ/email-templates/master/5/index.html',
  ],
  [
    'emailoctopus-abacus-transactional',
    'https://raw.githubusercontent.com/threeheartsdigital/emailoctopus-templates/master/abacus/transactional.html',
  ],
  [
    'emailoctopus-karakol-transactional',
    'https://raw.githubusercontent.com/threeheartsdigital/emailoctopus-templates/master/karakol/transactional.html',
  ],
  [
    'emailoctopus-wayfair-transactional',
    'https://raw.githubusercontent.com/threeheartsdigital/emailoctopus-templates/master/wayfair/transactional.html',
  ],
  [
    'codedmails-welcome-aleos',
    'https://raw.githubusercontent.com/hunzaboy/CodedMailsFree/master/html/welcome-email-aleos.html',
  ],
  [
    'codedmails-reset-dineos',
    'https://raw.githubusercontent.com/hunzaboy/CodedMailsFree/master/html/reset-email-dineos.html',
  ],
  [
    'codedmails-receipt-faedra',
    'https://raw.githubusercontent.com/hunzaboy/CodedMailsFree/master/html/receipt-email-faedra.html',
  ],
  [
    'codedmails-notification-ormes',
    'https://raw.githubusercontent.com/hunzaboy/CodedMailsFree/master/html/notification-email-ormes.html',
  ],
  [
    'cerberus-fluid',
    'https://raw.githubusercontent.com/emailmonday/Cerberus/master/cerberus-fluid.html',
  ],
  [
    'cerberus-hybrid',
    'https://raw.githubusercontent.com/emailmonday/Cerberus/master/cerberus-hybrid.html',
  ],
  [
    'cerberus-responsive',
    'https://raw.githubusercontent.com/emailmonday/Cerberus/master/cerberus-responsive.html',
  ],
  [
    'konsav-general',
    'https://raw.githubusercontent.com/konsav/email-templates/master/general.html',
  ],
  [
    'konsav-promotional',
    'https://raw.githubusercontent.com/konsav/email-templates/master/promotional.html',
  ],
  [
    'konsav-explorational',
    'https://raw.githubusercontent.com/konsav/email-templates/master/explorational.html',
  ],
  [
    'inkandthunder-notification',
    'https://raw.githubusercontent.com/inkandthunder/email-templates/master/notification.html',
  ],
  [
    'stripo-mothers-day-childhood-memory',
    'https://viewstripo.email/preview/web/template/cf99cd31-85de-4587-9948-e6ee48298491',
  ],
  [
    'stripo-mothers-day-thank-for-everything',
    'https://viewstripo.email/preview/web/template/d540384c-5ad3-42d9-8c12-d3ecd3662c57',
  ],
  [
    'stripo-mothers-day-help-mama',
    'https://viewstripo.email/preview/web/template/754bd273-5595-490d-9a87-e47a60db2380',
  ],
  [
    'stripo-mothers-day-gadgets-story',
    'https://viewstripo.email/preview/web/template/f895dacc-6a5f-4b59-915f-918a2804d1a9',
  ],
  [
    'stripo-mothers-day-best-flavors',
    'https://viewstripo.email/preview/web/template/6e4c7cf8-df48-4ee1-8f53-e3d303e85e81',
  ],
  [
    'stripo-promo-make-your-mom-happy',
    'https://viewstripo.email/preview/web/template/cf9f8bca-81eb-40cb-b6b5-f26d8369826e',
  ],
];

const TEMPLATE_METADATA = {
  'sendgrid-dynamic-receipt': {
    status: 'known-warning',
    expectedWarnings: 1,
    reason: 'contains unresolved {{this.image}} template data',
  },
  'colorlib-template-2': {
    status: 'known-warning',
    expectedWarnings: 3,
    reason: 'upstream fixture references images missing from the repository',
  },
  'colorlib-template-5': {
    status: 'known-warning',
    expectedWarnings: 3,
    reason: 'upstream fixture references images missing from the repository',
  },
  'codedmails-welcome-aleos': {
    status: 'known-warning',
    supportTier: 'legacy-hacks',
    expectedWarnings: 5,
    reason: 'upstream fixture uses relative image URLs that resolve outside the repository',
    supportReason: 'uses older email-hack patterns outside the modern support target',
  },
  'codedmails-reset-dineos': {
    status: 'known-warning',
    supportTier: 'legacy-hacks',
    expectedWarnings: 1,
    reason: 'upstream fixture uses relative image URLs that resolve outside the repository',
    supportReason: 'uses older email-hack patterns outside the modern support target',
  },
  'codedmails-receipt-faedra': {
    status: 'known-warning',
    supportTier: 'legacy-hacks',
    expectedWarnings: 3,
    reason: 'upstream fixture uses relative image URLs that resolve outside the repository',
    supportReason: 'uses older email-hack patterns outside the modern support target',
  },
  'codedmails-notification-ormes': {
    status: 'known-warning',
    supportTier: 'legacy-hacks',
    expectedWarnings: 5,
    reason: 'upstream fixture uses relative image URLs that resolve outside the repository',
    supportReason: 'uses older email-hack patterns outside the modern support target',
  },
  'cerberus-fluid': {
    status: 'known-warning',
    supportTier: 'legacy-hacks',
    expectedWarnings: 4,
    reason: 'upstream fixture depends on placeholder.com hero images that currently fail to load in corpus runs',
    supportReason: 'Cerberus relies on legacy hybrid/fluid email compatibility techniques outside the modern support target',
  },
  'cerberus-hybrid': {
    status: 'known-warning',
    supportTier: 'legacy-hacks',
    expectedWarnings: 9,
    reason: 'upstream fixture depends on placeholder.com hero images and currently exceeds semantic layout thresholds',
    supportReason: 'Cerberus relies on legacy hybrid/fluid email compatibility techniques outside the modern support target',
  },
  'cerberus-responsive': {
    status: 'known-warning',
    supportTier: 'legacy-hacks',
    expectedWarnings: 9,
    reason: 'upstream fixture depends on placeholder.com hero images and currently exceeds semantic layout thresholds',
    supportReason: 'Cerberus relies on legacy hybrid/fluid email compatibility techniques outside the modern support target',
  },
  'konsav-general': {
    status: 'known-warning',
    expectedWarnings: 0,
    reason: 'real remote-asset fixture currently exceeds semantic layout thresholds',
  },
  'konsav-promotional': {
    status: 'known-warning',
    expectedWarnings: 0,
    reason: 'real remote-asset fixture currently exceeds semantic layout thresholds',
  },
  'konsav-explorational': {
    status: 'known-warning',
    expectedWarnings: 0,
    reason: 'real remote-asset marketing fixture currently exceeds semantic layout thresholds',
  },
  'inkandthunder-notification': {
    status: 'known-warning',
    supportTier: 'invalid-structure',
    expectedWarnings: 0,
    reason: 'real remote-asset fixture currently exceeds semantic layout thresholds',
    supportReason: 'contains malformed table structure and browser-dependent DOM repair outside the supported HTML subset',
  },
};

const VENDORED_INDEX = new Map(vendoredCatalog.map((entry) => [entry.name, entry]));

const VENDORED_FIXTURES = vendoredCatalog.map((entry) => ({
  ...entry,
  preserveLocal: true,
}));

export const TEMPLATES = [
  ...REMOTE_TEMPLATES,
  ...VENDORED_FIXTURES.map((entry) => [entry.name, entry.url]),
];

export const TEMPLATE_CORPUS = VENDORED_FIXTURES.map((template) => ({
  ...template,
  ...(VENDORED_INDEX.get(template.name) ?? {}),
  provider: templateProvider(template.name),
  category: templateCategory(template.name),
  corpusGroup: templateCorpusGroup(template.name),
  supportTier: 'modern-supported',
  supportReason: '',
  status: 'active',
  expectedWarnings: 0,
  reason: '',
  ...(TEMPLATE_METADATA[template.name] ?? {}),
}));

const TEMPLATE_INDEX = new Map(TEMPLATE_CORPUS.map((template) => [template.name, template]));

export function getTemplate(name) {
  return TEMPLATE_INDEX.get(name) ?? null;
}

export async function loadTemplateSource(templateOrName, timeoutMs = 30000) {
  const template =
    typeof templateOrName === 'string' ? getTemplate(templateOrName) : templateOrName;
  if (!template) {
    throw new Error(`unknown template: ${templateOrName}`);
  }

  if (template.sourcePath) {
    const htmlPath = path.resolve(ROOT_DIR, template.sourcePath);
    const html = await readFile(htmlPath, 'utf8');
    return {
      template,
      html,
      htmlPath,
      url: template.url,
      baseUrl:
        template.baseUrl ?? pathToFileURL(`${path.dirname(htmlPath)}${path.sep}`).href,
    };
  }

  const response = await fetch(template.url, { signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) {
    throw new Error(`${template.url}: ${response.status} ${response.statusText}`);
  }
  return {
    template,
    html: await response.text(),
    htmlPath: null,
    url: template.url,
    baseUrl: template.baseUrl ?? new URL('.', template.url).href,
  };
}

function templateProvider(name) {
  if (name.startsWith('waypoint-')) return 'waypoint';
  if (name.startsWith('beefree-')) return 'beefree';
  if (name.startsWith('reallygoodemails-')) return 'reallygoodemails';
  if (name.startsWith('stripo-')) return 'stripo';
  if (name.startsWith('mjml-')) return 'mjml';
  if (name.startsWith('postmark-')) return 'postmark';
  if (name.startsWith('mailersend-')) return 'mailersend';
  if (name.startsWith('mailpace-')) return 'mailpace';
  if (name.startsWith('sendgrid-')) return 'sendgrid';
  if (name.startsWith('mailgun-')) return 'mailgun';
  if (name.startsWith('ckissi-')) return 'ckissi';
  if (name.startsWith('colorlib-')) return 'colorlib';
  if (name.startsWith('codedmails-')) return 'codedmails';
  if (name.startsWith('cerberus-')) return 'cerberus';
  if (name.startsWith('emailoctopus-')) return 'emailoctopus';
  if (name.startsWith('inkandthunder-')) return 'inkandthunder';
  if (name.startsWith('konsav-')) return 'konsav';
  if (name.startsWith('davidamunga-')) return 'davidamunga';
  return name.split('-')[0] || 'unknown';
}

function templateCorpusGroup(name) {
  const provider = templateProvider(name);
  if (
    [
      'leemunroe',
      'mailgun',
      'waypoint',
      'mailpace',
      'postmark',
      'mailersend',
      'sendgrid',
      'emailoctopus',
      'mjml',
    ].includes(provider)
  ) {
    return 'golden';
  }
  if (provider === 'reallygoodemails') {
    return 'research';
  }
  if (provider === 'cerberus') {
    return 'legacy-reference';
  }
  return 'real-world-dirty';
}

function templateCategory(name) {
  if (name.startsWith('beefree-')) {
    return beefreeCategory(name);
  }
  if (name.startsWith('stripo-') || name.startsWith('mjml-')) {
    return 'marketing';
  }
  if (name.includes('receipt') || name.includes('invoice') || name.includes('billing')) {
    return 'receipt';
  }
  if (name.includes('password') || name.includes('reset') || name.includes('otp')) {
    return 'account-security';
  }
  if (name.includes('welcome') || name.includes('invite') || name.includes('confirmation')) {
    return 'onboarding';
  }
  if (name.includes('trial') || name.includes('dunning') || name.includes('payment')) {
    return 'lifecycle';
  }
  if (name.includes('ecommerce') || name.includes('order') || name.includes('delivery')) {
    return 'commerce';
  }
  if (name.includes('marketplace')) {
    return 'marketplace';
  }
  if (name.includes('social')) {
    return 'social';
  }
  return 'general';
}

function beefreeCategory(name) {
  if (name.includes('terms') || name.includes('fine-print') || name.includes('feedback')) {
    return 'lifecycle';
  }
  if (name.includes('journey-in-review') || name.includes('monthly') || name.includes('voice')) {
    return 'newsletter';
  }
  if (name.includes('last-step')) {
    return 'onboarding';
  }
  if (name.includes('empty')) {
    return 'general';
  }
  return 'marketing';
}
