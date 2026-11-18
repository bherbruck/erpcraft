export const paths = {
  signIn: () => "/sign-in",
  salesOrders: (company: string) => `/c/${encodeURIComponent(company)}/sales-orders`,
  salesOrder: (company: string, id: string) => `/c/${encodeURIComponent(company)}/sales-orders/${id}`,
  newSalesOrder: (company: string) => `/c/${encodeURIComponent(company)}/sales-orders/new`,
  customers: (company: string) => `/c/${encodeURIComponent(company)}/customers`,
  customer: (company: string, id: string) => `/c/${encodeURIComponent(company)}/customers/${id}`,
  newCustomer: (company: string) => `/c/${encodeURIComponent(company)}/customers/new`,
};
